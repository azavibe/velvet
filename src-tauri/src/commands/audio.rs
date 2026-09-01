use super::ResultExt;
use crate::audio::{AudioDevice, AudioRecorder, RecordingState};
use crate::tray::{self, RecordingSource};
use serde::Serialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, State};

#[derive(Clone, Serialize)]
struct AudioLevelPayload {
    level: f32,
}

#[derive(Clone, Serialize)]
struct RecordingErrorPayload {
    error: String,
}

#[tauri::command]
pub fn list_audio_devices() -> Result<Vec<AudioDevice>, String> {
    AudioRecorder::list_devices().str_err()
}

#[tauri::command]
pub fn start_recording(
    app: AppHandle,
    state: State<'_, RecordingState>,
    device_id: Option<String>,
) -> Result<(), String> {
    AudioRecorder::start(&state, device_id).str_err()?;
    let tray_generation = tray::start_recording(&app, RecordingSource::Dictation);

    // Clone the Arc handles we need for the level emitter
    let (is_recording, peak_level, recording_error) = state.level_emitter_handles();
    let app_for_emitter = app.clone();

    // Spawn a thread to emit audio level events while recording
    let emitter = std::thread::Builder::new()
        .name("whisperi-audio-level".to_string())
        .spawn(move || {
            while is_recording.load(Ordering::SeqCst) {
                let level = *peak_level.lock().unwrap();
                let _ = app_for_emitter.emit("audio-level", AudioLevelPayload { level });

                // Check for recording errors
                if let Some(error) = recording_error.lock().unwrap().clone() {
                    let _ =
                        app_for_emitter.emit("recording-error", RecordingErrorPayload { error });
                    break;
                }

                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            // Emit a final zero level when recording stops
            let _ = app_for_emitter.emit("audio-level", AudioLevelPayload { level: 0.0 });
            tray::stop_recording(
                &app_for_emitter,
                RecordingSource::Dictation,
                Some(tray_generation),
            );
        });

    if let Err(error) = emitter {
        let _ = AudioRecorder::stop(&state);
        tray::stop_recording(&app, RecordingSource::Dictation, Some(tray_generation));
        return Err(format!("Failed to spawn level emitter: {error}"));
    }

    Ok(())
}

#[tauri::command]
pub fn stop_recording(app: AppHandle, state: State<'_, RecordingState>) -> Result<Vec<u8>, String> {
    let result = AudioRecorder::stop(&state).str_err();
    tray::stop_recording(&app, RecordingSource::Dictation, None);
    result
}

#[tauri::command]
pub fn get_audio_level(state: State<'_, RecordingState>) -> Result<f32, String> {
    Ok(state.get_level())
}
