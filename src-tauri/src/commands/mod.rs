pub mod app;
pub mod audio;
pub mod changelog;
pub mod clipboard;
pub mod conversation;
pub mod database;
pub mod live;
pub mod local_models;
pub mod notes;
pub mod playback;
pub mod reasoning;
pub mod settings;
pub mod transcription;

pub(crate) trait ResultExt<T> {
    fn str_err(self) -> Result<T, String>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for Result<T, E> {
    fn str_err(self) -> Result<T, String> {
        self.map_err(|e| e.to_string())
    }
}

/// `{app_data}/recordings/`, created on first use. Shared by dictation,
/// conversation, and note audio archiving (History & Analytics).
pub(crate) fn recordings_dir<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let base = app.path().app_data_dir().str_err()?;
    let dir = base.join("recordings");
    std::fs::create_dir_all(&dir).str_err()?;
    Ok(dir)
}

/// `{app_data}/models/`, created on first use. Local transcription models
/// are never placed beside application binaries or user recordings.
pub(crate) fn models_dir<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let base = app.path().app_data_dir().str_err()?;
    let dir = base.join("models");
    std::fs::create_dir_all(&dir).str_err()?;
    Ok(dir)
}
