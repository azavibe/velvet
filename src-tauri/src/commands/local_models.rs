use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncWriteExt;

use super::{ResultExt, models_dir};

#[derive(Debug, Clone, Copy)]
struct CatalogModel {
    id: &'static str,
    name: &'static str,
    filename: &'static str,
    size_bytes: u64,
    sha256: &'static str,
    quantization: &'static str,
    english_only: bool,
}

const CATALOG: &[CatalogModel] = &[
    CatalogModel {
        id: "tiny",
        name: "Whisper Tiny",
        filename: "ggml-tiny.bin",
        size_bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        quantization: "F16",
        english_only: false,
    },
    CatalogModel {
        id: "base",
        name: "Whisper Base",
        filename: "ggml-base.bin",
        size_bytes: 147_964_211,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        quantization: "F16",
        english_only: false,
    },
    CatalogModel {
        id: "small",
        name: "Whisper Small",
        filename: "ggml-small.bin",
        size_bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        quantization: "F16",
        english_only: false,
    },
    CatalogModel {
        id: "large-v3-turbo-q5_0",
        name: "Whisper Large v3 Turbo",
        filename: "ggml-large-v3-turbo-q5_0.bin",
        size_bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        quantization: "Q5_0",
        english_only: false,
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct LocalModelInfo {
    id: String,
    name: String,
    size_bytes: u64,
    downloaded_bytes: u64,
    quantization: String,
    english_only: bool,
    installed: bool,
    downloading: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DownloadProgress {
    model_id: String,
    downloaded_bytes: u64,
    total_bytes: u64,
    status: &'static str,
}

#[derive(Default)]
pub struct LocalModelDownloadState {
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn catalog_model(id: &str) -> Result<&'static CatalogModel, String> {
    CATALOG
        .iter()
        .find(|model| model.id == id)
        .ok_or_else(|| "unknown_local_model".to_string())
}

fn model_paths(root: &Path, model: &CatalogModel) -> (PathBuf, PathBuf, PathBuf) {
    let final_path = root.join(model.filename);
    let partial_path = root.join(format!("{}.part", model.filename));
    let marker_path = root.join(format!("{}.verified", model.filename));
    (final_path, partial_path, marker_path)
}

fn is_verified(final_path: &Path, marker_path: &Path, model: &CatalogModel) -> bool {
    final_path
        .metadata()
        .is_ok_and(|meta| meta.len() == model.size_bytes)
        && std::fs::read_to_string(marker_path).is_ok_and(|value| value.trim() == model.sha256)
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).str_err()?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).str_err()?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn remove_active(state: &LocalModelDownloadState, model_id: &str) {
    state.active.lock().unwrap().remove(model_id);
}

#[tauri::command]
pub fn list_local_models(
    app: AppHandle,
    state: State<'_, LocalModelDownloadState>,
) -> Result<Vec<LocalModelInfo>, String> {
    let root = models_dir(&app)?;
    let active = state.active.lock().unwrap();
    Ok(CATALOG
        .iter()
        .map(|model| {
            let (final_path, partial_path, marker_path) = model_paths(&root, model);
            LocalModelInfo {
                id: model.id.into(),
                name: model.name.into(),
                size_bytes: model.size_bytes,
                downloaded_bytes: if is_verified(&final_path, &marker_path, model) {
                    model.size_bytes
                } else {
                    partial_path.metadata().map(|meta| meta.len()).unwrap_or(0)
                },
                quantization: model.quantization.into(),
                english_only: model.english_only,
                installed: is_verified(&final_path, &marker_path, model),
                downloading: active.contains_key(model.id),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn download_local_model(
    app: AppHandle,
    state: State<'_, LocalModelDownloadState>,
    model_id: String,
) -> Result<(), String> {
    let model = *catalog_model(&model_id)?;
    let root = models_dir(&app)?;
    let (final_path, partial_path, marker_path) = model_paths(&root, &model);
    if is_verified(&final_path, &marker_path, &model) {
        return Ok(());
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = state.active.lock().unwrap();
        if active.contains_key(&model_id) {
            return Err("local_model_download_active".to_string());
        }
        active.insert(model_id.clone(), cancel.clone());
    }

    let result = async {
        let mut downloaded = partial_path.metadata().map(|meta| meta.len()).unwrap_or(0);
        if downloaded > model.size_bytes {
            let _ = tokio::fs::remove_file(&partial_path).await;
            downloaded = 0;
        }
        let required = model.size_bytes.saturating_sub(downloaded);
        let available = fs2::available_space(&root).str_err()?;
        if available < required.saturating_add(50 * 1024 * 1024) {
            return Err("local_model_insufficient_space".to_string());
        }

        let url = format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}?download=true",
            model.filename
        );
        let client = reqwest::Client::builder()
            .user_agent("Aral local model downloader")
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| "local_model_download_client".to_string())?;
        let mut request = client.get(url);
        if downloaded > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={downloaded}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| "local_model_download_network".to_string())?;
        let resumed = downloaded > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !response.status().is_success() {
            return Err(format!(
                "local_model_download_http_{}",
                response.status().as_u16()
            ));
        }
        if downloaded > 0 && !resumed {
            downloaded = 0;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).write(true);
        if resumed {
            options.append(true);
        } else {
            options.truncate(true);
        }
        let mut file = options
            .open(&partial_path)
            .await
            .map_err(|_| "local_model_download_write".to_string())?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Acquire) {
                file.flush().await.ok();
                return Err("local_model_download_cancelled".to_string());
            }
            let bytes = chunk.map_err(|_| "local_model_download_network".to_string())?;
            file.write_all(&bytes)
                .await
                .map_err(|_| "local_model_download_write".to_string())?;
            downloaded = downloaded.saturating_add(bytes.len() as u64);
            let _ = app.emit(
                "local-model-download-progress",
                DownloadProgress {
                    model_id: model_id.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes: model.size_bytes,
                    status: "downloading",
                },
            );
        }
        file.flush()
            .await
            .map_err(|_| "local_model_download_write".to_string())?;
        drop(file);
        if downloaded != model.size_bytes {
            return Err("local_model_download_truncated".to_string());
        }

        let verify_path = partial_path.clone();
        let digest = tauri::async_runtime::spawn_blocking(move || hash_file(&verify_path))
            .await
            .map_err(|_| "local_model_verify_failed".to_string())??;
        if digest != model.sha256 {
            let _ = tokio::fs::remove_file(&partial_path).await;
            return Err("local_model_checksum_mismatch".to_string());
        }
        let _ = tokio::fs::remove_file(&final_path).await;
        tokio::fs::rename(&partial_path, &final_path)
            .await
            .map_err(|_| "local_model_install_failed".to_string())?;
        tokio::fs::write(&marker_path, model.sha256)
            .await
            .map_err(|_| "local_model_install_failed".to_string())?;
        let _ = app.emit(
            "local-model-download-progress",
            DownloadProgress {
                model_id: model_id.clone(),
                downloaded_bytes: model.size_bytes,
                total_bytes: model.size_bytes,
                status: "installed",
            },
        );
        Ok(())
    }
    .await;
    remove_active(&state, &model_id);
    result
}

#[tauri::command]
pub fn cancel_local_model_download(
    state: State<'_, LocalModelDownloadState>,
    model_id: String,
) -> Result<(), String> {
    let active = state.active.lock().unwrap();
    let cancel = active
        .get(&model_id)
        .ok_or_else(|| "local_model_download_not_active".to_string())?;
    cancel.store(true, Ordering::Release);
    Ok(())
}

#[tauri::command]
pub fn delete_local_model(
    app: AppHandle,
    state: State<'_, LocalModelDownloadState>,
    model_id: String,
) -> Result<(), String> {
    let model = catalog_model(&model_id)?;
    if state.active.lock().unwrap().contains_key(&model_id) {
        return Err("local_model_download_active".to_string());
    }
    let root = models_dir(&app)?;
    let (final_path, partial_path, marker_path) = model_paths(&root, model);
    for path in [final_path, partial_path, marker_path] {
        if path.exists() {
            std::fs::remove_file(path).str_err()?;
        }
    }
    let _ = app.emit(
        "local-model-download-progress",
        DownloadProgress {
            model_id,
            downloaded_bytes: 0,
            total_bytes: model.size_bytes,
            status: "deleted",
        },
    );
    Ok(())
}

pub fn installed_model_path(app: &AppHandle, model_id: &str) -> Result<PathBuf, String> {
    let model = catalog_model(model_id)?;
    let root = models_dir(app)?;
    let (final_path, _, marker_path) = model_paths(&root, model);
    if !is_verified(&final_path, &marker_path, model) {
        return Err("local_model_not_installed".to_string());
    }
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_and_filenames_are_unique_and_safe() {
        let mut ids = std::collections::HashSet::new();
        let mut files = std::collections::HashSet::new();
        for model in CATALOG {
            assert!(ids.insert(model.id));
            assert!(files.insert(model.filename));
            assert!(!model.filename.contains('/') && !model.filename.contains('\\'));
            assert_eq!(model.sha256.len(), 64);
            assert!(model.size_bytes > 1_000_000);
        }
    }

    #[test]
    fn sha256_verification_hashes_file_content() {
        let path = std::env::temp_dir().join(format!("aral-hash-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            hash_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(path).unwrap();
    }
}
