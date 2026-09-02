pub mod migrations;
pub mod word_count;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use crate::audio::archive::{self, ArchiveFailure};

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AudioAssetStatus {
    Saving,
    Ready,
    Missing,
    Failed,
}

impl AudioAssetStatus {
    fn from_db(value: &str) -> Self {
        match value {
            "saving" => Self::Saving,
            "ready" => Self::Ready,
            "missing" => Self::Missing,
            "failed" => Self::Failed,
            _ => Self::Failed,
        }
    }

    fn as_db(self) -> &'static str {
        match self {
            Self::Saving => "saving",
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioAsset {
    pub id: i64,
    pub owner_type: String,
    pub owner_id: i64,
    pub channel: String,
    pub sequence: i64,
    pub status: AudioAssetStatus,
    /// Only ready assets expose a path. The database may retain a path for
    /// recovery/deletion while an asset is saving or has failed.
    pub path: Option<String>,
    pub byte_length: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcription {
    pub id: i64,
    pub timestamp: String,
    pub original_text: String,
    pub processed_text: Option<String>,
    pub reconciled_text: Option<String>,
    pub reconciliation_status: String,
    pub reconciliation_confidence: Option<f64>,
    pub reconciliation_evidence: Option<String>,
    pub is_processed: bool,
    pub processing_method: String,
    pub agent_name: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
    pub word_count: Option<i64>,
    /// Compatibility path for the next History milestone. It is only
    /// populated when `audio_asset` is ready.
    pub audio_path: Option<String>,
    pub audio_asset: Option<AudioAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryCandidate {
    pub kind: String,
    pub canonical_text: String,
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryContextItem {
    pub id: i64,
    pub kind: String,
    pub canonical_text: String,
    pub aliases: Vec<String>,
    pub confidence: f64,
    pub support_count: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum StatsPeriod {
    Today,
    Week,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsPayload {
    pub total_seconds: i64,
    pub total_words: i64,
    pub total_recordings: i64,
    pub avg_seconds: f64,
    pub avg_words: f64,
}

/// Privacy-safe summary of the one-time archive recovery pass. Paths and
/// user content are intentionally excluded from the report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioRecoveryReport {
    pub checked: u64,
    pub saving: u64,
    pub ready: u64,
    pub missing: u64,
    pub failed: u64,
}

/// Summary row for the Conversations history list — no utterances/suggestions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub persona_name: Option<String>,
    /// Compatibility paths for the next History milestone. They are only
    /// populated when the matching asset is ready.
    pub audio_path_me: Option<String>,
    pub audio_path_them: Option<String>,
    pub audio_asset_me: Option<AudioAsset>,
    pub audio_asset_them: Option<AudioAsset>,
    /// First utterance's text, for a collapsed-card preview without
    /// fetching the full transcript. `None` for a conversation with no
    /// utterances (e.g. started and immediately stopped).
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationUtterance {
    pub id: i64,
    pub conversation_id: i64,
    /// "me" (microphone) or "them" (system-audio loopback).
    pub channel: String,
    pub started_at_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSuggestion {
    pub id: i64,
    pub conversation_id: i64,
    pub created_at_ms: i64,
    pub persona_name: Option<String>,
    pub text: String,
}

/// Full detail view for one conversation: the parent row plus its
/// utterances and suggestions, each already ordered for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDetail {
    pub conversation: ConversationSummary,
    pub utterances: Vec<ConversationUtterance>,
    pub suggestions: Vec<ConversationSuggestion>,
}

/// A Note: mic-only capture with inline editing and an optional AI
/// Markdown cleanup pass. `raw_transcript` is what capture produced and is
/// never overwritten by cleanup; `body_markdown` is the opt-in cleaned-up
/// version, `None` until the user runs cleanup. History cards show
/// `body_markdown` when present, falling back to `raw_transcript`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub created_at: String,
    pub updated_at: String,
    pub title: Option<String>,
    pub raw_transcript: String,
    pub body_markdown: Option<String>,
    pub audio_path: Option<String>,
    /// Every recorded append segment, ordered by capture sequence.
    pub audio_segments: Vec<AudioAsset>,
    pub tags: Vec<String>,
}

/// Initialize the database and store it in Tauri's managed state
pub fn init(app: &AppHandle) -> Result<()> {
    #[cfg(debug_assertions)]
    let started = std::time::Instant::now();
    let db_path = get_db_path(app)?;
    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

    #[cfg(debug_assertions)]
    log::info!("[startup] database opened");

    // Required for `ON DELETE CASCADE` on conversation_utterances/conversation_suggestions
    // (v3 migration) to actually cascade — SQLite ignores FK constraints unless this
    // pragma is set per-connection.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    migrations::run(&conn)?;

    #[cfg(debug_assertions)]
    log::info!(
        "[startup] database migrations complete elapsed_ms={}",
        started.elapsed().as_millis()
    );

    let recordings_dir = app.path().app_data_dir()?.join("recordings");
    std::fs::create_dir_all(&recordings_dir)?;
    let database = Database {
        conn: Arc::new(Mutex::new(conn)),
    };
    app.manage(database.clone());

    // Migrations and directory creation are required before the UI can use
    // the database. Full WAV validation is not: rows remain explicitly
    // saving/missing/failed until this bounded recovery pass finishes, while
    // the recording protocol validates a requested ready asset again.
    let recovery_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(debug_assertions)]
        let recovery_started = std::time::Instant::now();
        #[cfg(debug_assertions)]
        log::info!("[startup] audio recovery started");

        match database.reconcile_audio_assets(&recordings_dir, true) {
            Ok(report) => {
                #[cfg(debug_assertions)]
                log::info!(
                    "[startup] audio recovery complete checked={} saving={} ready={} missing={} failed={} elapsed_ms={}",
                    report.checked,
                    report.saving,
                    report.ready,
                    report.missing,
                    report.failed,
                    recovery_started.elapsed().as_millis()
                );
                let _ = recovery_app.emit("audio-recovery-complete", report);
            }
            Err(_) => {
                #[cfg(debug_assertions)]
                log::info!(
                    "[startup] audio recovery failed elapsed_ms={}",
                    recovery_started.elapsed().as_millis()
                );
                let _ = recovery_app.emit("audio-recovery-failed", "audio_recovery_failed");
            }
        }
    });

    #[cfg(debug_assertions)]
    log::info!("[startup] database ready for UI");

    Ok(())
}

fn get_db_path(app: &AppHandle) -> Result<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .context("Failed to resolve app data directory")?;
    std::fs::create_dir_all(&app_data)?;
    Ok(app_data.join("whisperi.db"))
}

impl Database {
    /// Create an in-memory database with migrations applied. Test-only.
    #[cfg(test)]
    pub(crate) fn new_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrations::run(&conn)?;
        Ok(Database {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn row_to_audio_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<AudioAsset> {
        let raw_path: String = row.get(5)?;
        let status = AudioAssetStatus::from_db(row.get::<_, String>(6)?.as_str());
        Ok(AudioAsset {
            id: row.get(0)?,
            owner_type: row.get(1)?,
            owner_id: row.get(2)?,
            channel: row.get(3)?,
            sequence: row.get(4)?,
            path: (status == AudioAssetStatus::Ready && !raw_path.is_empty()).then_some(raw_path),
            status,
            byte_length: row.get(7)?,
            duration_ms: row.get(8)?,
            error: row.get(9)?,
        })
    }

    /// Load one asset by its opaque database id for the recording protocol.
    /// Paths remain hidden for non-ready assets and are revalidated by the
    /// protocol before every request.
    pub fn get_audio_asset(&self, asset_id: i64) -> Result<Option<AudioAsset>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, owner_type, owner_id, channel, sequence, path, status,
                    byte_length, duration_ms, error
             FROM audio_assets
             WHERE id = ?1",
        )?;
        let mut rows = stmt.query([asset_id])?;
        rows.next()?
            .map(Self::row_to_audio_asset)
            .transpose()
            .map_err(Into::into)
    }

    fn load_audio_asset_conn(
        conn: &Connection,
        owner_type: &str,
        owner_id: i64,
        channel: &str,
    ) -> Result<Option<AudioAsset>> {
        let mut stmt = conn.prepare(
            "SELECT id, owner_type, owner_id, channel, sequence, path, status,
                    byte_length, duration_ms, error
             FROM audio_assets
             WHERE owner_type = ?1 AND owner_id = ?2 AND channel = ?3
             ORDER BY sequence DESC, id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![owner_type, owner_id, channel])?;
        rows.next()?
            .map(Self::row_to_audio_asset)
            .transpose()
            .map_err(Into::into)
    }

    fn load_audio_assets_conn(
        conn: &Connection,
        owner_type: &str,
        owner_id: i64,
        channel: &str,
    ) -> Result<Vec<AudioAsset>> {
        let mut stmt = conn.prepare(
            "SELECT id, owner_type, owner_id, channel, sequence, path, status,
                    byte_length, duration_ms, error
             FROM audio_assets
             WHERE owner_type = ?1 AND owner_id = ?2 AND channel = ?3
             ORDER BY sequence ASC, id ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![owner_type, owner_id, channel],
            Self::row_to_audio_asset,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn audio_paths_conn(conn: &Connection, owner_type: &str, owner_id: i64) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT path FROM audio_assets
             WHERE owner_type = ?1 AND owner_id = ?2 AND path <> ''",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner_type, owner_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn update_legacy_path_conn(
        conn: &Connection,
        owner_type: &str,
        owner_id: i64,
        channel: &str,
    ) -> Result<()> {
        let path: Option<String> = conn
            .query_row(
                "SELECT path FROM audio_assets
                 WHERE owner_type = ?1 AND owner_id = ?2 AND channel = ?3
                   AND status = 'ready' AND path <> ''
                 ORDER BY sequence DESC, id DESC LIMIT 1",
                rusqlite::params![owner_type, owner_id, channel],
                |row| row.get(0),
            )
            .ok();

        match (owner_type, channel) {
            ("dictation", "main") => {
                conn.execute(
                    "UPDATE transcriptions SET audio_path = ?2 WHERE id = ?1",
                    rusqlite::params![owner_id, path],
                )?;
            }
            ("conversation", "me") => {
                conn.execute(
                    "UPDATE conversations SET audio_path_me = ?2 WHERE id = ?1",
                    rusqlite::params![owner_id, path],
                )?;
            }
            ("conversation", "them") => {
                conn.execute(
                    "UPDATE conversations SET audio_path_them = ?2 WHERE id = ?1",
                    rusqlite::params![owner_id, path],
                )?;
            }
            ("note", "main") => {
                conn.execute(
                    "UPDATE notes SET audio_path = ?2 WHERE id = ?1",
                    rusqlite::params![owner_id, path],
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Create the durable `saving` row before any bytes are written. The
    /// caller sets the final path before starting the file operation.
    pub fn begin_audio_asset(
        &self,
        owner_type: &str,
        owner_id: i64,
        channel: &str,
    ) -> Result<(i64, i64)> {
        let conn = self.conn.lock().unwrap();
        let owner_exists = match owner_type {
            "dictation" => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM transcriptions WHERE id = ?1)",
                [owner_id],
                |row| row.get::<_, bool>(0),
            )?,
            "conversation" => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1)",
                [owner_id],
                |row| row.get::<_, bool>(0),
            )?,
            "note" => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1)",
                [owner_id],
                |row| row.get::<_, bool>(0),
            )?,
            _ => false,
        };
        if !owner_exists {
            anyhow::bail!("audio owner does not exist");
        }
        let sequence: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1
             FROM audio_assets WHERE owner_type = ?1 AND owner_id = ?2 AND channel = ?3",
            rusqlite::params![owner_type, owner_id, channel],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO audio_assets
                (owner_type, owner_id, channel, sequence, status)
             VALUES (?1, ?2, ?3, ?4, 'saving')",
            rusqlite::params![owner_type, owner_id, channel, sequence],
        )?;
        Ok((conn.last_insert_rowid(), sequence))
    }

    pub fn set_audio_asset_path(&self, asset_id: i64, path: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE audio_assets SET path = ?2 WHERE id = ?1",
            rusqlite::params![asset_id, path.to_string_lossy().as_ref()],
        )?;
        Ok(())
    }

    pub fn mark_audio_asset_ready(
        &self,
        asset_id: i64,
        byte_length: i64,
        duration_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let (owner_type, owner_id, channel): (String, i64, String) = conn.query_row(
            "SELECT owner_type, owner_id, channel FROM audio_assets WHERE id = ?1",
            [asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        conn.execute(
            "UPDATE audio_assets
             SET status = 'ready', byte_length = ?2, duration_ms = ?3, error = NULL
             WHERE id = ?1",
            rusqlite::params![asset_id, byte_length, duration_ms],
        )?;
        Self::update_legacy_path_conn(&conn, &owner_type, owner_id, &channel)?;
        Ok(())
    }

    pub fn mark_audio_asset_failed(&self, asset_id: i64, error: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let (owner_type, owner_id, channel): (String, i64, String) = conn.query_row(
            "SELECT owner_type, owner_id, channel FROM audio_assets WHERE id = ?1",
            [asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        conn.execute(
            "UPDATE audio_assets
             SET status = 'failed', byte_length = NULL, duration_ms = NULL, error = ?2
             WHERE id = ?1",
            rusqlite::params![asset_id, error],
        )?;
        Self::update_legacy_path_conn(&conn, &owner_type, owner_id, &channel)?;
        Ok(())
    }

    /// Make a ready asset's disappearance durable without deleting its row.
    /// The stored path remains available to source-aware cleanup, while
    /// History can report the missing state on its next refresh.
    pub fn mark_audio_asset_missing(&self, asset_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let (owner_type, owner_id, channel): (String, i64, String) = conn.query_row(
            "SELECT owner_type, owner_id, channel FROM audio_assets WHERE id = ?1",
            [asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        conn.execute(
            "UPDATE audio_assets
             SET status = 'missing', byte_length = NULL, duration_ms = NULL, error = 'file_missing'
             WHERE id = ?1 AND status = 'ready'",
            [asset_id],
        )?;
        Self::update_legacy_path_conn(&conn, &owner_type, owner_id, &channel)?;
        Ok(())
    }

    fn set_audio_asset_status(
        &self,
        asset_id: i64,
        expected_status: AudioAssetStatus,
        status: AudioAssetStatus,
        error: Option<&str>,
        metadata: Option<archive::WavMetadata>,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let Some((owner_type, owner_id, channel)): Option<(String, i64, String)> = conn
            .query_row(
                "SELECT owner_type, owner_id, channel FROM audio_assets WHERE id = ?1",
                [asset_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
        else {
            // A source can be deleted while the recovery pass is validating
            // its file. That is a successful no-op, not a failed recovery.
            return Ok(false);
        };
        let (byte_length, duration_ms) = metadata
            .map(|m| (Some(m.byte_length), Some(m.duration_ms)))
            .unwrap_or((None, None));
        let changed = conn.execute(
            "UPDATE audio_assets
             SET status = ?2, byte_length = ?3, duration_ms = ?4, error = ?5
             WHERE id = ?1 AND status = ?6",
            rusqlite::params![
                asset_id,
                status.as_db(),
                byte_length,
                duration_ms,
                error,
                expected_status.as_db(),
            ],
        )?;
        if changed == 1 {
            Self::update_legacy_path_conn(&conn, &owner_type, owner_id, &channel)?;
        }
        Ok(changed == 1)
    }

    /// Recover interrupted writes, validate legacy files, and downgrade
    /// deleted/corrupt files. Filesystem work is performed without holding
    /// the database mutex; each update is conditional on the status observed
    /// in the snapshot so active writers cannot be overwritten by stale work.
    pub fn reconcile_audio_assets(
        &self,
        recordings_root: &Path,
        recover_interrupted: bool,
    ) -> Result<AudioRecoveryReport> {
        let assets = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, owner_type, owner_id, channel, path, status
                 FROM audio_assets ORDER BY id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut report = AudioRecoveryReport::default();

        for (asset_id, _owner_type, _owner_id, _channel, raw_path, raw_status) in assets {
            report.checked += 1;
            let status = AudioAssetStatus::from_db(&raw_status);
            if status == AudioAssetStatus::Failed {
                report.failed += 1;
                continue;
            }

            if raw_path.is_empty() {
                if status == AudioAssetStatus::Saving && !recover_interrupted {
                    report.saving += 1;
                    continue;
                }
                let error = if status == AudioAssetStatus::Saving {
                    ArchiveFailure::Interrupted.code()
                } else {
                    ArchiveFailure::InvalidPath.code()
                };
                if self.set_audio_asset_status(
                    asset_id,
                    status,
                    AudioAssetStatus::Failed,
                    Some(error),
                    None,
                )? {
                    report.failed += 1;
                }
                continue;
            }

            let Some(path) = archive::validated_recording_path(recordings_root, &raw_path) else {
                if self.set_audio_asset_status(
                    asset_id,
                    status,
                    AudioAssetStatus::Failed,
                    Some(ArchiveFailure::InvalidPath.code()),
                    None,
                )? {
                    report.failed += 1;
                }
                continue;
            };

            if status == AudioAssetStatus::Saving {
                let temp = archive::temp_path(&path);
                if path.exists() {
                    match archive::validate_wav_file(&path) {
                        Ok(metadata) => {
                            let _ = std::fs::remove_file(&temp);
                            if self.set_audio_asset_status(
                                asset_id,
                                status,
                                AudioAssetStatus::Ready,
                                None,
                                Some(metadata),
                            )? {
                                report.ready += 1;
                            }
                        }
                        Err(error) => {
                            let _ = std::fs::remove_file(&temp);
                            if self.set_audio_asset_status(
                                asset_id,
                                status,
                                AudioAssetStatus::Failed,
                                Some(error.code()),
                                None,
                            )? {
                                report.failed += 1;
                            }
                        }
                    }
                } else if recover_interrupted {
                    let _ =
                        archive::remove_recording_if_safe(recordings_root, &temp.to_string_lossy());
                    if self.set_audio_asset_status(
                        asset_id,
                        status,
                        AudioAssetStatus::Failed,
                        Some(ArchiveFailure::Interrupted.code()),
                        None,
                    )? {
                        report.failed += 1;
                    }
                } else {
                    report.saving += 1;
                }
                continue;
            }

            if !path.exists() {
                if self.set_audio_asset_status(
                    asset_id,
                    status,
                    AudioAssetStatus::Missing,
                    Some("file_missing"),
                    None,
                )? {
                    report.missing += 1;
                }
                continue;
            }

            match archive::validate_wav_file(&path) {
                Ok(metadata) => {
                    let _ = std::fs::remove_file(archive::temp_path(&path));
                    if self.set_audio_asset_status(
                        asset_id,
                        status,
                        AudioAssetStatus::Ready,
                        None,
                        Some(metadata),
                    )? {
                        report.ready += 1;
                    }
                }
                Err(error) => {
                    if self.set_audio_asset_status(
                        asset_id,
                        status,
                        AudioAssetStatus::Failed,
                        Some(error.code()),
                        None,
                    )? {
                        report.failed += 1;
                    }
                }
            }
        }
        Ok(report)
    }

    #[cfg(test)]
    pub fn save_transcription(
        &self,
        original_text: &str,
        processed_text: Option<&str>,
        processing_method: &str,
        agent_name: Option<&str>,
        error: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<i64> {
        self.save_transcription_with_reconciliation(
            original_text,
            processed_text,
            processing_method,
            agent_name,
            error,
            duration_ms,
            None,
            "disabled",
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_transcription_with_reconciliation(
        &self,
        original_text: &str,
        processed_text: Option<&str>,
        processing_method: &str,
        agent_name: Option<&str>,
        error: Option<&str>,
        duration_ms: Option<i64>,
        reconciled_text: Option<&str>,
        reconciliation_status: &str,
        reconciliation_confidence: Option<f64>,
        reconciliation_evidence: Option<&str>,
    ) -> Result<i64> {
        // Count words on the final user-visible text — processed_text when AI
        // enhancement is on, otherwise the raw transcription.
        let counted_text = processed_text.unwrap_or(original_text);
        let word_count = word_count::count_words(counted_text);

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcriptions
               (original_text, processed_text, is_processed, processing_method,
                agent_name, error, duration_ms, word_count, reconciled_text,
                reconciliation_status, reconciliation_confidence, reconciliation_evidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                original_text,
                processed_text,
                processed_text.is_some(),
                processing_method,
                agent_name,
                error,
                duration_ms,
                word_count,
                reconciled_text,
                reconciliation_status,
                reconciliation_confidence,
                reconciliation_evidence,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_transcriptions(&self, limit: u32, offset: u32) -> Result<Vec<Transcription>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, original_text, processed_text, is_processed, processing_method, agent_name, error, duration_ms, word_count, audio_path,
                    reconciled_text, reconciliation_status, reconciliation_confidence, reconciliation_evidence
             FROM transcriptions ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![limit, offset], |row| {
            Ok(Transcription {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                original_text: row.get(2)?,
                processed_text: row.get(3)?,
                is_processed: row.get(4)?,
                processing_method: row.get(5)?,
                agent_name: row.get(6)?,
                error: row.get(7)?,
                duration_ms: row.get(8)?,
                word_count: row.get(9)?,
                audio_path: None,
                audio_asset: None,
                reconciled_text: row.get(11)?,
                reconciliation_status: row.get(12)?,
                reconciliation_confidence: row.get(13)?,
                reconciliation_evidence: row.get(14)?,
            })
        })?;

        let mut transcriptions = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for transcription in &mut transcriptions {
            let asset = Self::load_audio_asset_conn(&conn, "dictation", transcription.id, "main")?;
            transcription.audio_path = asset.as_ref().and_then(|asset| asset.path.clone());
            transcription.audio_asset = asset;
        }
        Ok(transcriptions)
    }

    pub fn complete_transcription_retry(
        &self,
        id: i64,
        original_text: &str,
        provider: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            !original_text.trim().is_empty(),
            "empty retry transcription"
        );
        let word_count = word_count::count_words(original_text);
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE transcriptions
             SET original_text = ?2, processed_text = NULL, is_processed = 0,
                 processing_method = ?3, error = NULL, word_count = ?4,
                 reconciled_text = NULL, reconciliation_status = 'disabled',
                 reconciliation_confidence = NULL, reconciliation_evidence = NULL
             WHERE id = ?1 AND error IS NOT NULL",
            rusqlite::params![id, original_text, format!("retry:{provider}"), word_count],
        )?;
        anyhow::ensure!(changed == 1, "transcription is not retryable");
        Ok(())
    }

    pub fn replace_transcription(&self, id: i64, original_text: &str, method: &str) -> Result<()> {
        anyhow::ensure!(!original_text.trim().is_empty(), "empty transcription");
        let word_count = word_count::count_words(original_text);
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE transcriptions
             SET original_text = ?2, processed_text = NULL, is_processed = 0,
                 processing_method = ?3, error = NULL, word_count = ?4,
                 reconciled_text = NULL, reconciliation_status = 'disabled',
                 reconciliation_confidence = NULL, reconciliation_evidence = NULL
             WHERE id = ?1",
            rusqlite::params![id, original_text, method, word_count],
        )?;
        anyhow::ensure!(changed == 1, "transcription not found");
        Ok(())
    }

    pub fn store_memory_candidates(
        &self,
        source_type: &str,
        source_id: i64,
        candidates: &[MemoryCandidate],
    ) -> Result<u32> {
        anyhow::ensure!(
            matches!(source_type, "dictation" | "conversation" | "note"),
            "invalid memory source type"
        );
        anyhow::ensure!(source_id > 0, "invalid memory source id");
        anyhow::ensure!(candidates.len() <= 20, "too many memory candidates");

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let source_exists: bool = match source_type {
            "dictation" => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM transcriptions WHERE id = ?1)",
                [source_id],
                |row| row.get(0),
            )?,
            "conversation" => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1)",
                [source_id],
                |row| row.get(0),
            )?,
            "note" => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1)",
                [source_id],
                |row| row.get(0),
            )?,
            _ => false,
        };
        anyhow::ensure!(source_exists, "memory source no longer exists");
        let mut stored = 0_u32;
        for candidate in candidates {
            let kind = candidate.kind.trim();
            let canonical = candidate.canonical_text.trim();
            if !matches!(kind, "entity" | "fact" | "relationship" | "summary")
                || canonical.is_empty()
                || canonical.chars().count() > 500
                || !candidate.confidence.is_finite()
                || !(0.6..=1.0).contains(&candidate.confidence)
            {
                continue;
            }
            let subject = candidate
                .subject
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let predicate = candidate
                .predicate
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let object = candidate
                .object
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());

            if matches!(kind, "fact" | "relationship")
                && candidate.confidence >= 0.8
                && let (Some(subject), Some(predicate), Some(object)) = (subject, predicate, object)
            {
                tx.execute(
                    "UPDATE memory_items
                     SET state = 'contradicted', updated_at = CURRENT_TIMESTAMP
                     WHERE kind IN ('fact', 'relationship')
                       AND lower(subject) = lower(?1)
                       AND lower(predicate) = lower(?2)
                       AND lower(COALESCE(object, '')) <> lower(?3)
                       AND lower(canonical_text) <> lower(?4)",
                    rusqlite::params![subject, predicate, object, canonical],
                )?;
            }

            let existing: Option<(i64, f64)> = tx
                .query_row(
                    "SELECT id, confidence FROM memory_items
                     WHERE lower(canonical_text) = lower(?1)",
                    [canonical],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (memory_id, previous_confidence) = match existing {
                Some(value) => value,
                None => {
                    tx.execute(
                        "INSERT INTO memory_items
                         (kind, canonical_text, subject, predicate, object, confidence)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            kind,
                            canonical,
                            subject,
                            predicate,
                            object,
                            candidate.confidence
                        ],
                    )?;
                    (tx.last_insert_rowid(), candidate.confidence)
                }
            };

            tx.execute(
                "INSERT OR IGNORE INTO memory_sources (memory_id, source_type, source_id)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![memory_id, source_type, source_id],
            )?;
            let source_added = tx.changes() > 0;
            for alias in candidate.aliases.iter().take(12) {
                let alias = alias.trim();
                if alias.is_empty() || alias.chars().count() > 120 {
                    continue;
                }
                tx.execute(
                    "INSERT OR IGNORE INTO memory_aliases (memory_id, alias) VALUES (?1, ?2)",
                    rusqlite::params![memory_id, alias],
                )?;
            }
            let support_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM memory_sources WHERE memory_id = ?1",
                [memory_id],
                |row| row.get(0),
            )?;
            let confidence = if source_added && support_count > 1 {
                1.0 - (1.0 - previous_confidence) * (1.0 - candidate.confidence * 0.5)
            } else {
                previous_confidence.max(candidate.confidence)
            };
            let has_conflict: bool = tx.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM memory_items other
                   WHERE other.id <> ?1
                     AND other.kind IN ('fact', 'relationship')
                     AND lower(other.subject) = lower(?2)
                     AND lower(other.predicate) = lower(?3)
                     AND lower(COALESCE(other.object, '')) <> lower(COALESCE(?4, ''))
                 )",
                rusqlite::params![memory_id, subject, predicate, object],
                |row| row.get(0),
            )?;
            let state = if has_conflict && matches!(kind, "fact" | "relationship") {
                "contradicted"
            } else if support_count >= 2 && confidence >= 0.85 {
                "confirmed"
            } else {
                "provisional"
            };
            tx.execute(
                "UPDATE memory_items
                 SET confidence = ?2, support_count = ?3, state = ?4,
                     updated_at = CURRENT_TIMESTAMP,
                     last_supported_at = CASE WHEN ?5 THEN CURRENT_TIMESTAMP ELSE last_supported_at END
                 WHERE id = ?1",
                rusqlite::params![memory_id, confidence, support_count, state, source_added],
            )?;
            stored += 1;
        }
        tx.commit()?;
        Ok(stored)
    }

    pub fn get_memory_context(&self, query: &str, limit: u32) -> Result<Vec<MemoryContextItem>> {
        let conn = self.conn.lock().unwrap();
        let bounded = limit.clamp(1, 12);
        let mut stmt = conn.prepare(
            "SELECT id, kind, canonical_text,
                    confidence * CASE
                      WHEN julianday('now') - julianday(last_supported_at) > 365 THEN 0.6
                      WHEN julianday('now') - julianday(last_supported_at) > 180 THEN 0.8
                      ELSE 1.0 END AS effective_confidence,
                    support_count
             FROM memory_items
             WHERE state = 'confirmed'
               AND confidence * CASE
                     WHEN julianday('now') - julianday(last_supported_at) > 365 THEN 0.6
                     WHEN julianday('now') - julianday(last_supported_at) > 180 THEN 0.8
                     ELSE 1.0 END >= 0.8
             ORDER BY
               CASE
                 WHEN lower(?1) LIKE '%' || lower(canonical_text) || '%' THEN 0
                 WHEN EXISTS (
                   SELECT 1 FROM memory_aliases a
                   WHERE a.memory_id = memory_items.id
                     AND lower(?1) LIKE '%' || lower(a.alias) || '%'
                 ) THEN 1
                 ELSE 2 END,
               last_supported_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![query, bounded], |row| {
            Ok(MemoryContextItem {
                id: row.get(0)?,
                kind: row.get(1)?,
                canonical_text: row.get(2)?,
                confidence: row.get(3)?,
                support_count: row.get(4)?,
                aliases: Vec::new(),
            })
        })?;
        let mut items = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for item in &mut items {
            let mut alias_stmt = conn.prepare(
                "SELECT alias FROM memory_aliases WHERE memory_id = ?1 ORDER BY id LIMIT 12",
            )?;
            item.aliases = alias_stmt
                .query_map([item.id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(items)
    }

    pub fn reset_memory(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM memory_items", [])?;
        Ok(())
    }

    fn detach_memory_source_conn(
        conn: &Connection,
        source_type: &str,
        source_id: i64,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT memory_id FROM memory_sources WHERE source_type = ?1 AND source_id = ?2",
        )?;
        let ids = stmt
            .query_map(rusqlite::params![source_type, source_id], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        conn.execute(
            "DELETE FROM memory_sources WHERE source_type = ?1 AND source_id = ?2",
            rusqlite::params![source_type, source_id],
        )?;
        for memory_id in ids {
            let support_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_sources WHERE memory_id = ?1",
                [memory_id],
                |row| row.get(0),
            )?;
            if support_count == 0 {
                conn.execute("DELETE FROM memory_items WHERE id = ?1", [memory_id])?;
            } else {
                conn.execute(
                    "UPDATE memory_items
                     SET support_count = ?2,
                         state = CASE WHEN state = 'contradicted' THEN state
                                      WHEN ?2 >= 2 AND confidence >= 0.85 THEN 'confirmed'
                                      ELSE 'provisional' END,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?1",
                    rusqlite::params![memory_id, support_count],
                )?;
            }
        }
        Self::recompute_memory_states_conn(conn)?;
        Ok(())
    }

    fn recompute_memory_states_conn(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE memory_items
             SET state = CASE
               WHEN kind IN ('fact', 'relationship') AND EXISTS (
                 SELECT 1 FROM memory_items other
                 WHERE other.id <> memory_items.id
                   AND other.kind IN ('fact', 'relationship')
                   AND lower(other.subject) = lower(memory_items.subject)
                   AND lower(other.predicate) = lower(memory_items.predicate)
                   AND lower(COALESCE(other.object, '')) <> lower(COALESCE(memory_items.object, ''))
               ) THEN 'contradicted'
               WHEN support_count >= 2 AND confidence >= 0.85 THEN 'confirmed'
               ELSE 'provisional' END,
             updated_at = CURRENT_TIMESTAMP",
            [],
        )?;
        Ok(())
    }

    fn detach_all_memory_sources_conn(conn: &Connection, source_type: &str) -> Result<()> {
        conn.execute(
            "DELETE FROM memory_sources WHERE source_type = ?1",
            [source_type],
        )?;
        conn.execute(
            "DELETE FROM memory_items
             WHERE NOT EXISTS (SELECT 1 FROM memory_sources WHERE memory_id = memory_items.id)",
            [],
        )?;
        conn.execute(
            "UPDATE memory_items
             SET support_count = (SELECT COUNT(*) FROM memory_sources WHERE memory_id = memory_items.id),
                 updated_at = CURRENT_TIMESTAMP",
            [],
        )?;
        Self::recompute_memory_states_conn(conn)?;
        Ok(())
    }

    /// Compatibility-only setter for legacy callers. New archive code uses
    /// `mark_audio_asset_ready`, which updates the state and this column
    /// together.
    #[allow(dead_code)]
    pub fn update_transcription_audio_path(&self, id: i64, path: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transcriptions SET audio_path = ?2 WHERE id = ?1",
            rusqlite::params![id, path],
        )?;
        Ok(())
    }

    /// Deletes the row and every linked recording. Missing files are safe;
    /// deletion is restricted to the validated recordings root.
    pub fn delete_transcription(&self, id: i64, recordings_root: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let legacy_path: Option<String> = conn
            .query_row(
                "SELECT audio_path FROM transcriptions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .ok();
        let mut paths = Self::audio_paths_conn(&conn, "dictation", id)?;
        if let Some(path) = legacy_path {
            paths.push(path);
        }
        Self::detach_memory_source_conn(&conn, "dictation", id)?;
        conn.execute("DELETE FROM transcriptions WHERE id = ?1", [id])?;
        conn.execute(
            "DELETE FROM audio_assets WHERE owner_type = 'dictation' AND owner_id = ?1",
            [id],
        )?;
        drop(conn);
        Self::remove_paths(recordings_root, paths);
        Ok(())
    }

    pub fn clear_transcriptions(&self, recordings_root: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut paths: Vec<String> = Vec::new();
        {
            let mut stmt =
                conn.prepare("SELECT audio_path FROM transcriptions WHERE audio_path IS NOT NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            paths.extend(rows.filter_map(|r| r.ok()));
        }
        {
            let mut stmt = conn.prepare(
                "SELECT path FROM audio_assets WHERE owner_type = 'dictation' AND path <> ''",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            paths.extend(rows.filter_map(|r| r.ok()));
        }
        Self::detach_all_memory_sources_conn(&conn, "dictation")?;
        conn.execute("DELETE FROM transcriptions", [])?;
        conn.execute(
            "DELETE FROM audio_assets WHERE owner_type = 'dictation'",
            [],
        )?;
        drop(conn);
        Self::remove_paths(recordings_root, paths);
        Ok(())
    }

    fn remove_paths(recordings_root: &Path, paths: Vec<String>) {
        if recordings_root.as_os_str().is_empty() {
            return;
        }
        let mut unique = HashSet::new();
        for path in paths {
            if path.is_empty() || !unique.insert(path.clone()) {
                continue;
            }
            let _ = archive::remove_recording_if_safe(recordings_root, &path);
            let temp = archive::temp_path(Path::new(&path));
            let _ = archive::remove_recording_if_safe(recordings_root, &temp.to_string_lossy());
        }
    }

    pub fn get_stats(&self, period: StatsPeriod) -> Result<StatsPayload> {
        // All queries filter `duration_ms IS NOT NULL` so pre-feature rows
        // (NULL after the v2 migration on an existing DB) are excluded from
        // both totals.
        //
        // For Today/Week we compute the cutoff in UTC up front rather than
        // wrapping the `timestamp` column in `datetime(..., 'localtime')`.
        // Stored timestamps come from `CURRENT_TIMESTAMP` (always UTC), so
        // comparing them against a precomputed UTC cutoff keeps the predicate
        // sargable — a future index on `timestamp` would actually be usable.
        //
        // Today cutoff = local midnight, expressed in UTC. The chained
        // modifiers do: UTC now → local → start of local day → back to UTC.
        // Week cutoff = "now − 7 days"; timezone math is irrelevant for a
        // pure 7×24h window, so the simpler UTC form is equivalent.
        let (sum_ms, sum_words, total_recordings): (i64, i64, i64) = {
            let conn = self.conn.lock().unwrap();
            match period {
                StatsPeriod::Today => conn.query_row(
                    "SELECT COALESCE(SUM(duration_ms), 0),
                            COALESCE(SUM(word_count), 0),
                            COUNT(*)
                     FROM transcriptions
                     WHERE duration_ms IS NOT NULL AND error IS NULL
                       AND timestamp >= datetime('now', 'localtime', 'start of day', 'utc')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
                StatsPeriod::Week => conn.query_row(
                    "SELECT COALESCE(SUM(duration_ms), 0),
                            COALESCE(SUM(word_count), 0),
                            COUNT(*)
                     FROM transcriptions
                     WHERE duration_ms IS NOT NULL AND error IS NULL
                       AND timestamp >= datetime('now', '-7 days')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
                StatsPeriod::All => conn.query_row(
                    "SELECT COALESCE(SUM(duration_ms), 0),
                            COALESCE(SUM(word_count), 0),
                            COUNT(*)
                     FROM transcriptions
                     WHERE duration_ms IS NOT NULL AND error IS NULL",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
            }
        };

        // Total uses integer seconds (matches what the UI renders).
        // Average is computed from sum_ms directly so we don't lose sub-second
        // precision: two 1.9 s recordings should average to ~1.9 s, not 1.5 s.
        let total_seconds = sum_ms / 1000;
        let (avg_seconds, avg_words) = if total_recordings > 0 {
            (
                (sum_ms as f64) / 1000.0 / (total_recordings as f64),
                (sum_words as f64) / (total_recordings as f64),
            )
        } else {
            (0.0, 0.0)
        };

        Ok(StatsPayload {
            total_seconds,
            total_words: sum_words,
            total_recordings,
            avg_seconds,
            avg_words,
        })
    }

    // --- Conversations ---

    pub fn create_conversation(&self, persona_name: Option<&str>) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (persona_name) VALUES (?1)",
            rusqlite::params![persona_name],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn end_conversation(&self, conversation_id: i64, title: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET ended_at = CURRENT_TIMESTAMP, title = COALESCE(?2, title) WHERE id = ?1",
            rusqlite::params![conversation_id, title],
        )?;
        Ok(())
    }

    pub fn insert_conversation_utterance(
        &self,
        conversation_id: i64,
        channel: &str,
        started_at_ms: i64,
        text: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversation_utterances (conversation_id, channel, started_at_ms, text)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation_id, channel, started_at_ms, text],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn insert_conversation_suggestion(
        &self,
        conversation_id: i64,
        created_at_ms: i64,
        persona_name: Option<&str>,
        text: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversation_suggestions (conversation_id, created_at_ms, persona_name, text)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation_id, created_at_ms, persona_name, text],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Utterances on `conversation_id` at or after `since_ms`, any channel,
    /// oldest first. Used for echo detection — checking whether a
    /// just-transcribed chunk on one channel is actually the other
    /// channel's audio bleeding into the microphone (speaker playback,
    /// not headphones).
    pub fn get_recent_utterances(
        &self,
        conversation_id: i64,
        since_ms: i64,
    ) -> Result<Vec<ConversationUtterance>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, channel, started_at_ms, text
             FROM conversation_utterances WHERE conversation_id = ?1 AND started_at_ms >= ?2
             ORDER BY started_at_ms ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![conversation_id, since_ms], |row| {
            Ok(ConversationUtterance {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                channel: row.get(2)?,
                started_at_ms: row.get(3)?,
                text: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_conversations(&self, limit: u32, offset: u32) -> Result<Vec<ConversationSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.started_at, c.ended_at, c.title, c.persona_name,
                    c.audio_path_me, c.audio_path_them,
                    (SELECT text FROM conversation_utterances u
                     WHERE u.conversation_id = c.id ORDER BY u.started_at_ms ASC, u.id ASC LIMIT 1) AS snippet
             FROM conversations c ORDER BY c.id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], |row| {
            Ok(ConversationSummary {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                title: row.get(3)?,
                persona_name: row.get(4)?,
                audio_path_me: None,
                audio_path_them: None,
                audio_asset_me: None,
                audio_asset_them: None,
                snippet: row.get(7)?,
            })
        })?;
        let mut conversations = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for conversation in &mut conversations {
            conversation.audio_asset_me =
                Self::load_audio_asset_conn(&conn, "conversation", conversation.id, "me")?;
            conversation.audio_asset_them =
                Self::load_audio_asset_conn(&conn, "conversation", conversation.id, "them")?;
            conversation.audio_path_me = conversation
                .audio_asset_me
                .as_ref()
                .and_then(|asset| asset.path.clone());
            conversation.audio_path_them = conversation
                .audio_asset_them
                .as_ref()
                .and_then(|asset| asset.path.clone());
        }
        Ok(conversations)
    }

    pub fn get_conversation(&self, conversation_id: i64) -> Result<ConversationDetail> {
        let conn = self.conn.lock().unwrap();

        let mut conversation = conn.query_row(
            "SELECT c.id, c.started_at, c.ended_at, c.title, c.persona_name,
                    c.audio_path_me, c.audio_path_them,
                    (SELECT text FROM conversation_utterances u
                     WHERE u.conversation_id = c.id ORDER BY u.started_at_ms ASC, u.id ASC LIMIT 1) AS snippet
             FROM conversations c WHERE c.id = ?1",
            rusqlite::params![conversation_id],
            |row| {
                Ok(ConversationSummary {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    ended_at: row.get(2)?,
                    title: row.get(3)?,
                    persona_name: row.get(4)?,
                    audio_path_me: None,
                    audio_path_them: None,
                    audio_asset_me: None,
                    audio_asset_them: None,
                    snippet: row.get(7)?,
                })
            },
        )?;

        conversation.audio_asset_me =
            Self::load_audio_asset_conn(&conn, "conversation", conversation.id, "me")?;
        conversation.audio_asset_them =
            Self::load_audio_asset_conn(&conn, "conversation", conversation.id, "them")?;
        conversation.audio_path_me = conversation
            .audio_asset_me
            .as_ref()
            .and_then(|asset| asset.path.clone());
        conversation.audio_path_them = conversation
            .audio_asset_them
            .as_ref()
            .and_then(|asset| asset.path.clone());

        let utterances = {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, channel, started_at_ms, text
                 FROM conversation_utterances WHERE conversation_id = ?1 ORDER BY started_at_ms ASC, id ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![conversation_id], |row| {
                Ok(ConversationUtterance {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    channel: row.get(2)?,
                    started_at_ms: row.get(3)?,
                    text: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let suggestions = {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, created_at_ms, persona_name, text
                 FROM conversation_suggestions WHERE conversation_id = ?1 ORDER BY created_at_ms ASC, id ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![conversation_id], |row| {
                Ok(ConversationSuggestion {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    created_at_ms: row.get(2)?,
                    persona_name: row.get(3)?,
                    text: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        Ok(ConversationDetail {
            conversation,
            utterances,
            suggestions,
        })
    }

    /// Set once each channel's audio has been archived to disk (see
    /// `ConversationAudioArchive` in commands/conversation.rs). Either path
    /// may be `None` if that channel never captured anything, so both are
    /// updated independently via `COALESCE` rather than overwriting a path
    /// already recorded for the other channel.
    #[allow(dead_code)]
    pub fn update_conversation_audio_paths(
        &self,
        conversation_id: i64,
        audio_path_me: Option<&str>,
        audio_path_them: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET
                audio_path_me = COALESCE(?2, audio_path_me),
                audio_path_them = COALESCE(?3, audio_path_them)
             WHERE id = ?1",
            rusqlite::params![conversation_id, audio_path_me, audio_path_them],
        )?;
        Ok(())
    }

    /// Deletes the row (and its utterances/suggestions) and, best-effort,
    /// both archived audio files. A missing file is not an error.
    pub fn delete_conversation(&self, conversation_id: i64, recordings_root: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let legacy_paths: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT audio_path_me, audio_path_them FROM conversations WHERE id = ?1",
                rusqlite::params![conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let mut paths = Self::audio_paths_conn(&conn, "conversation", conversation_id)?;
        if let Some((me, them)) = legacy_paths {
            if let Some(path) = me {
                paths.push(path);
            }
            if let Some(path) = them {
                paths.push(path);
            }
        }

        Self::detach_memory_source_conn(&conn, "conversation", conversation_id)?;

        // Explicit cascade, not just relying on the FK pragma: belt-and-braces
        // in case a future connection opens without `PRAGMA foreign_keys = ON`.
        conn.execute(
            "DELETE FROM conversation_suggestions WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM conversation_utterances WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM conversations WHERE id = ?1",
            rusqlite::params![conversation_id],
        )?;
        conn.execute(
            "DELETE FROM audio_assets WHERE owner_type = 'conversation' AND owner_id = ?1",
            rusqlite::params![conversation_id],
        )?;
        drop(conn);
        Self::remove_paths(recordings_root, paths);
        Ok(())
    }

    // --- Notes ---

    fn row_to_note(row: &rusqlite::Row) -> rusqlite::Result<Note> {
        let tags_json: String = row.get(6)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        Ok(Note {
            id: row.get(0)?,
            created_at: row.get(1)?,
            updated_at: row.get(2)?,
            title: row.get(3)?,
            raw_transcript: row.get(4)?,
            body_markdown: row.get(5)?,
            tags,
            audio_path: None,
            audio_segments: Vec::new(),
        })
    }

    const NOTE_COLUMNS: &'static str =
        "id, created_at, updated_at, title, raw_transcript, body_markdown, tags, audio_path";

    pub fn create_note(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT INTO notes DEFAULT VALUES", [])?;
        Ok(conn.last_insert_rowid())
    }

    /// Appends transcribed text to a note's running transcript — used both
    /// for the live capture stream and for "Append Dictation" on an
    /// existing note. Bumps `updated_at` so the note resurfaces at the top
    /// of the newest-first history list.
    pub fn append_note_transcript(&self, note_id: i64, text: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET
                raw_transcript = CASE WHEN raw_transcript = '' THEN ?2 ELSE raw_transcript || ' ' || ?2 END,
                updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            rusqlite::params![note_id, text],
        )?;
        Ok(())
    }

    /// Inline edit: overwrites title/transcript with user-provided text.
    /// `body_markdown` isn't touched here — editing the raw transcript
    /// after a cleanup pass leaves the cleaned version as-is until the user
    /// re-runs cleanup, rather than silently invalidating it.
    pub fn update_note(
        &self,
        note_id: i64,
        title: Option<&str>,
        raw_transcript: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET title = ?2, raw_transcript = ?3, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            rusqlite::params![note_id, title, raw_transcript],
        )?;
        Ok(())
    }

    /// Title-only update — used by the live capture window, which doesn't
    /// have (and shouldn't need to reconstruct) the authoritative
    /// server-side transcript text that `update_note` would otherwise
    /// overwrite.
    pub fn set_note_title(&self, note_id: i64, title: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET title = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            rusqlite::params![note_id, title],
        )?;
        Ok(())
    }

    pub fn set_note_markdown(&self, note_id: i64, body_markdown: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET body_markdown = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            rusqlite::params![note_id, body_markdown],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn update_note_audio_path(&self, note_id: i64, audio_path: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET audio_path = ?2 WHERE id = ?1",
            rusqlite::params![note_id, audio_path],
        )?;
        Ok(())
    }

    pub fn list_notes(&self, limit: u32, offset: u32) -> Result<Vec<Note>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            Self::NOTE_COLUMNS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], Self::row_to_note)?;
        let mut notes = rows.collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for note in &mut notes {
            Self::attach_note_audio(&conn, note)?;
        }
        Ok(notes)
    }

    pub fn get_note(&self, note_id: i64) -> Result<Note> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {} FROM notes WHERE id = ?1", Self::NOTE_COLUMNS);
        let mut note = conn
            .query_row(&sql, rusqlite::params![note_id], Self::row_to_note)
            .map_err(anyhow::Error::from)?;
        Self::attach_note_audio(&conn, &mut note)?;
        Ok(note)
    }

    /// Deletes the row and every linked audio segment. Missing files are
    /// harmless; paths outside the recordings root are never removed.
    pub fn delete_note(&self, note_id: i64, recordings_root: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let legacy_path: Option<String> = conn
            .query_row(
                "SELECT audio_path FROM notes WHERE id = ?1",
                [note_id],
                |r| r.get(0),
            )
            .ok();
        let mut paths = Self::audio_paths_conn(&conn, "note", note_id)?;
        if let Some(path) = legacy_path {
            paths.push(path);
        }
        Self::detach_memory_source_conn(&conn, "note", note_id)?;
        conn.execute("DELETE FROM notes WHERE id = ?1", [note_id])?;
        conn.execute(
            "DELETE FROM audio_assets WHERE owner_type = 'note' AND owner_id = ?1",
            [note_id],
        )?;
        drop(conn);
        Self::remove_paths(recordings_root, paths);
        Ok(())
    }

    fn attach_note_audio(conn: &Connection, note: &mut Note) -> Result<()> {
        note.audio_segments = Self::load_audio_assets_conn(conn, "note", note.id, "main")?;
        note.audio_path = note
            .audio_segments
            .iter()
            .rev()
            .find_map(|asset| asset.path.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn insert_raw(
        db: &Database,
        original: &str,
        duration_ms: Option<i64>,
        word_count: Option<i64>,
        timestamp_sql: &str,
    ) {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcriptions (timestamp, original_text, duration_ms, word_count)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![timestamp_sql, original, duration_ms, word_count],
        )
        .unwrap();
    }

    #[test]
    fn save_transcription_persists_word_count_and_duration() {
        let db = Database::new_in_memory().unwrap();
        let id = db
            .save_transcription("hello world", None, "none", None, None, Some(2500))
            .unwrap();
        assert!(id > 0);

        let conn = db.conn.lock().unwrap();
        let (dur, wc): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT duration_ms, word_count FROM transcriptions WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(dur, Some(2500));
        assert_eq!(wc, Some(2));
    }

    #[test]
    fn save_transcription_counts_processed_text_when_present() {
        let db = Database::new_in_memory().unwrap();
        let id = db
            .save_transcription(
                "um hello",
                Some("hello world"),
                "ai",
                None,
                None,
                Some(1000),
            )
            .unwrap();
        let conn = db.conn.lock().unwrap();
        let wc: i64 = conn
            .query_row(
                "SELECT word_count FROM transcriptions WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        // Counts processed_text ("hello world") = 2, not original ("um hello") = 2.
        // Same value here by coincidence — assert nonzero to prove it ran.
        assert_eq!(wc, 2);
    }

    #[test]
    fn reconciliation_round_trip_preserves_raw_and_audit_fields() {
        let db = Database::new_in_memory().unwrap();
        let id = db
            .save_transcription_with_reconciliation(
                "Schedule Acme tomorrow",
                Some("Schedule Acme Corp tomorrow."),
                "reconciliation+ai",
                Some("Aral"),
                None,
                Some(1200),
                Some("Schedule Acme Corp tomorrow"),
                "accepted",
                Some(0.97),
                Some("recent"),
            )
            .unwrap();

        let rows = db.get_transcriptions(10, 0).unwrap();
        let item = rows.iter().find(|item| item.id == id).unwrap();
        assert_eq!(item.original_text, "Schedule Acme tomorrow");
        assert_eq!(
            item.reconciled_text.as_deref(),
            Some("Schedule Acme Corp tomorrow")
        );
        assert_eq!(
            item.processed_text.as_deref(),
            Some("Schedule Acme Corp tomorrow.")
        );
        assert_eq!(item.reconciliation_status, "accepted");
        assert_eq!(item.reconciliation_confidence, Some(0.97));
        assert_eq!(item.reconciliation_evidence.as_deref(), Some("recent"));
    }

    #[test]
    fn failed_transcription_retry_updates_the_existing_row() {
        let db = Database::new_in_memory().unwrap();
        let id = db
            .save_transcription(
                "",
                None,
                "failed",
                None,
                Some("transcription_failed"),
                Some(2200),
            )
            .unwrap();

        db.complete_transcription_retry(id, "Recovered words", "groq")
            .unwrap();
        let item = db
            .get_transcriptions(10, 0)
            .unwrap()
            .into_iter()
            .find(|item| item.id == id)
            .unwrap();
        assert_eq!(item.original_text, "Recovered words");
        assert_eq!(item.error, None);
        assert_eq!(item.processing_method, "retry:groq");
        assert_eq!(item.word_count, Some(2));
        assert!(
            db.complete_transcription_retry(id, "again", "groq")
                .is_err()
        );
    }

    fn acme_memory(object: &str) -> MemoryCandidate {
        MemoryCandidate {
            kind: "fact".into(),
            canonical_text: format!("Oscar works at {object}"),
            subject: Some("Oscar".into()),
            predicate: Some("works at".into()),
            object: Some(object.into()),
            aliases: vec![object.into()],
            confidence: 0.92,
        }
    }

    #[test]
    fn memory_requires_independent_support_and_downgrades_on_source_delete() {
        let db = Database::new_in_memory().unwrap();
        let first = db
            .save_transcription("I work at Acme", None, "none", None, None, None)
            .unwrap();
        let second = db
            .save_transcription("My employer is Acme", None, "none", None, None, None)
            .unwrap();
        db.store_memory_candidates("dictation", first, &[acme_memory("Acme")])
            .unwrap();
        assert!(db.get_memory_context("Acme", 12).unwrap().is_empty());

        db.store_memory_candidates("dictation", second, &[acme_memory("Acme")])
            .unwrap();
        let confirmed = db.get_memory_context("Acme", 12).unwrap();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].support_count, 2);

        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE memory_items SET last_supported_at = datetime('now', '-181 days')",
                [],
            )
            .unwrap();
        assert!(db.get_memory_context("Acme", 12).unwrap().is_empty());
        db.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE memory_items SET last_supported_at = CURRENT_TIMESTAMP",
                [],
            )
            .unwrap();

        db.delete_transcription(first, Path::new("")).unwrap();
        assert!(
            db.store_memory_candidates("dictation", first, &[acme_memory("Late result")])
                .is_err()
        );
        assert!(db.get_memory_context("Acme", 12).unwrap().is_empty());
        let state: (String, i64) = db
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT state, support_count FROM memory_items", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(state, ("provisional".into(), 1));
    }

    #[test]
    fn conflicting_memory_marks_prior_fact_contradicted_and_reset_clears_all() {
        let db = Database::new_in_memory().unwrap();
        let first = db
            .save_transcription("I work at Acme", None, "none", None, None, None)
            .unwrap();
        let second = db
            .save_transcription("Acme is my employer", None, "none", None, None, None)
            .unwrap();
        let note = db.create_note().unwrap();
        db.store_memory_candidates("dictation", first, &[acme_memory("Acme")])
            .unwrap();
        db.store_memory_candidates("dictation", second, &[acme_memory("Acme")])
            .unwrap();
        db.store_memory_candidates("note", note, &[acme_memory("Globex")])
            .unwrap();
        let state: String = db
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM memory_items WHERE canonical_text = 'Oscar works at Acme'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "contradicted");
        db.delete_note(note, Path::new("")).unwrap();
        let restored = db.get_memory_context("Acme", 12).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].support_count, 2);
        db.reset_memory().unwrap();
        let count: i64 = db
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM memory_items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn get_stats_all_excludes_null_duration_rows() {
        let db = Database::new_in_memory().unwrap();
        // Pre-feature row (no duration recorded).
        insert_raw(&db, "old", None, None, "2026-01-01 12:00:00");
        // Two new-feature rows.
        insert_raw(&db, "new1", Some(2000), Some(3), "2026-05-19 09:00:00");
        insert_raw(&db, "new2", Some(4000), Some(5), "2026-05-19 09:01:00");

        let s = db.get_stats(StatsPeriod::All).unwrap();
        assert_eq!(s.total_recordings, 2);
        assert_eq!(s.total_seconds, 6);
        assert_eq!(s.total_words, 8);
        assert!((s.avg_seconds - 3.0).abs() < 1e-9);
        assert!((s.avg_words - 4.0).abs() < 1e-9);
    }

    #[test]
    fn get_stats_excludes_failed_recordings_with_audio_duration() {
        let db = Database::new_in_memory().unwrap();
        db.save_transcription("successful words", None, "none", None, None, Some(2000))
            .unwrap();
        db.save_transcription(
            "",
            None,
            "failed",
            None,
            Some("transcription_failed"),
            Some(9000),
        )
        .unwrap();

        let stats = db.get_stats(StatsPeriod::All).unwrap();
        assert_eq!(stats.total_recordings, 1);
        assert_eq!(stats.total_seconds, 2);
        assert_eq!(stats.total_words, 2);
    }

    #[test]
    fn get_stats_empty_db_returns_zero_averages() {
        let db = Database::new_in_memory().unwrap();
        let s = db.get_stats(StatsPeriod::All).unwrap();
        assert_eq!(s.total_recordings, 0);
        assert_eq!(s.total_seconds, 0);
        assert_eq!(s.total_words, 0);
        assert_eq!(s.avg_seconds, 0.0);
        assert_eq!(s.avg_words, 0.0);
    }

    #[test]
    fn get_stats_today_uses_localtime_midnight() {
        let db = Database::new_in_memory().unwrap();
        // datetime('now') is UTC; we just want a row that is unambiguously "today" locally.
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcriptions (timestamp, original_text, duration_ms, word_count)
             VALUES (datetime('now'), 'just now', 1500, 2)",
            [],
        )
        .unwrap();
        // Old row from years ago.
        conn.execute(
            "INSERT INTO transcriptions (timestamp, original_text, duration_ms, word_count)
             VALUES ('2020-01-01 12:00:00', 'long ago', 9999, 999)",
            [],
        )
        .unwrap();
        drop(conn);

        let today = db.get_stats(StatsPeriod::Today).unwrap();
        assert_eq!(today.total_recordings, 1);
        assert_eq!(today.total_seconds, 1);
        assert_eq!(today.total_words, 2);
    }

    #[test]
    fn get_stats_week_excludes_rows_older_than_seven_days() {
        let db = Database::new_in_memory().unwrap();
        let conn = db.conn.lock().unwrap();
        // 3 days ago — should be included.
        conn.execute(
            "INSERT INTO transcriptions (timestamp, original_text, duration_ms, word_count)
             VALUES (datetime('now', '-3 days'), 'recent', 2000, 4)",
            [],
        )
        .unwrap();
        // 30 days ago — should be excluded.
        conn.execute(
            "INSERT INTO transcriptions (timestamp, original_text, duration_ms, word_count)
             VALUES (datetime('now', '-30 days'), 'old', 9000, 100)",
            [],
        )
        .unwrap();
        drop(conn);

        let week = db.get_stats(StatsPeriod::Week).unwrap();
        assert_eq!(week.total_recordings, 1);
        assert_eq!(week.total_seconds, 2);
        assert_eq!(week.total_words, 4);
    }

    #[test]
    fn conversation_round_trip_orders_by_started_at_ms() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_conversation(Some("Sales")).unwrap();

        // Insert out of wall-clock order to prove the query sorts by
        // started_at_ms, not insertion order — the two capture channels
        // finish transcribing independently and can arrive in either order.
        db.insert_conversation_utterance(id, "them", 2000, "so what's your budget")
            .unwrap();
        db.insert_conversation_utterance(id, "me", 1000, "hi thanks for taking the call")
            .unwrap();
        db.insert_conversation_suggestion(id, 2500, Some("Sales"), "mention the annual discount")
            .unwrap();

        db.end_conversation(id, Some("Discovery call")).unwrap();

        let detail = db.get_conversation(id).unwrap();
        assert_eq!(detail.conversation.title.as_deref(), Some("Discovery call"));
        assert!(detail.conversation.ended_at.is_some());
        assert_eq!(detail.utterances.len(), 2);
        assert_eq!(detail.utterances[0].channel, "me");
        assert_eq!(detail.utterances[1].channel, "them");
        assert_eq!(detail.suggestions.len(), 1);
    }

    #[test]
    fn delete_conversation_cascades_utterances_and_suggestions() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_conversation(None).unwrap();
        db.insert_conversation_utterance(id, "me", 0, "hello")
            .unwrap();
        db.insert_conversation_suggestion(id, 0, None, "say hi back")
            .unwrap();

        db.delete_conversation(id, Path::new("")).unwrap();

        let conn = db.conn.lock().unwrap();
        let conversations: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        let utterances: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_utterances", [], |r| {
                r.get(0)
            })
            .unwrap();
        let suggestions: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_suggestions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(conversations, 0);
        assert_eq!(utterances, 0);
        assert_eq!(suggestions, 0);
    }

    #[test]
    fn list_conversations_orders_newest_first() {
        let db = Database::new_in_memory().unwrap();
        let first = db.create_conversation(None).unwrap();
        let second = db.create_conversation(None).unwrap();

        let list = db.list_conversations(10, 0).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second);
        assert_eq!(list[1].id, first);
    }

    #[test]
    fn get_recent_utterances_filters_by_time_and_includes_both_channels() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_conversation(None).unwrap();
        db.insert_conversation_utterance(id, "me", 1000, "old one")
            .unwrap();
        db.insert_conversation_utterance(id, "them", 5000, "recent them")
            .unwrap();
        db.insert_conversation_utterance(id, "me", 5200, "recent me")
            .unwrap();

        let recent = db.get_recent_utterances(id, 4000).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].text, "recent them");
        assert_eq!(recent[1].text, "recent me");
    }

    #[test]
    fn create_note_starts_empty_with_default_fields() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_note().unwrap();
        let note = db.get_note(id).unwrap();
        assert_eq!(note.title, None);
        assert_eq!(note.raw_transcript, "");
        assert_eq!(note.body_markdown, None);
        assert_eq!(note.audio_path, None);
        assert!(note.tags.is_empty());
    }

    #[test]
    fn append_note_transcript_joins_chunks_with_a_space() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_note().unwrap();
        db.append_note_transcript(id, "hello").unwrap();
        db.append_note_transcript(id, "world").unwrap();
        let note = db.get_note(id).unwrap();
        assert_eq!(note.raw_transcript, "hello world");
    }

    #[test]
    fn update_note_overwrites_title_and_transcript_but_not_markdown() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_note().unwrap();
        db.append_note_transcript(id, "raw text").unwrap();
        db.set_note_markdown(id, "# Title\n\ncleaned up").unwrap();

        db.update_note(id, Some("My title"), "edited raw text")
            .unwrap();

        let note = db.get_note(id).unwrap();
        assert_eq!(note.title.as_deref(), Some("My title"));
        assert_eq!(note.raw_transcript, "edited raw text");
        assert_eq!(note.body_markdown.as_deref(), Some("# Title\n\ncleaned up"));
    }

    #[test]
    fn set_note_title_does_not_touch_transcript() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_note().unwrap();
        db.append_note_transcript(id, "keep me").unwrap();
        db.set_note_title(id, "Renamed").unwrap();

        let note = db.get_note(id).unwrap();
        assert_eq!(note.title.as_deref(), Some("Renamed"));
        assert_eq!(note.raw_transcript, "keep me");
    }

    #[test]
    fn list_notes_orders_most_recently_updated_first() {
        let db = Database::new_in_memory().unwrap();
        let first = db.create_note().unwrap();
        let second = db.create_note().unwrap();
        // Touch the first note again so it should resurface at the top.
        db.append_note_transcript(first, "later addition").unwrap();

        let list = db.list_notes(10, 0).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, first);
        assert_eq!(list[1].id, second);
    }

    #[test]
    fn delete_note_removes_the_row() {
        let db = Database::new_in_memory().unwrap();
        let id = db.create_note().unwrap();
        db.delete_note(id, Path::new("")).unwrap();
        assert!(db.get_note(id).is_err());
    }

    fn audio_test_root(label: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agenda-db-audio-{label}-{suffix}"));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_wav() -> Vec<u8> {
        crate::audio::recorder::encode_wav(&[0.1, -0.1, 0.2, -0.2], 16_000).unwrap()
    }

    fn create_ready_asset(
        db: &Database,
        owner_type: &str,
        owner_id: i64,
        channel: &str,
        path: &Path,
    ) -> (i64, i64) {
        let (asset_id, sequence) = db.begin_audio_asset(owner_type, owner_id, channel).unwrap();
        db.set_audio_asset_path(asset_id, path).unwrap();
        let metadata = crate::audio::archive::write_wav_atomically(path, &test_wav()).unwrap();
        db.mark_audio_asset_ready(asset_id, metadata.byte_length, metadata.duration_ms)
            .unwrap();
        (asset_id, sequence)
    }

    #[test]
    fn audio_asset_states_hide_incomplete_paths_and_reconcile_writes() {
        let db = Database::new_in_memory().unwrap();
        let root = audio_test_root("states");

        let saving_id = db
            .save_transcription("saving", None, "none", None, None, None)
            .unwrap();
        let (saving_asset_id, _) = db
            .begin_audio_asset("dictation", saving_id, "main")
            .unwrap();
        let saving_path = root.join("saving.wav");
        db.set_audio_asset_path(saving_asset_id, &saving_path)
            .unwrap();

        let saving = db.get_transcriptions(10, 0).unwrap().remove(0);
        let saving_asset = saving.audio_asset.unwrap();
        assert_eq!(saving_asset.status, AudioAssetStatus::Saving);
        assert_eq!(saving_asset.path, None);

        // History validation must not mistake a live `.part` file for an
        // interrupted write. Startup passes `true` below; live reads pass
        // `false` so the active consumer remains the single owner.
        let saving_temp = crate::audio::archive::temp_path(&saving_path);
        std::fs::write(&saving_temp, &test_wav()[..8]).unwrap();
        db.reconcile_audio_assets(&root, false).unwrap();
        assert!(saving_temp.exists());
        assert_eq!(
            db.get_transcriptions(10, 0)
                .unwrap()
                .remove(0)
                .audio_asset
                .unwrap()
                .status,
            AudioAssetStatus::Saving
        );
        std::fs::remove_file(&saving_temp).unwrap();

        crate::audio::archive::write_wav_atomically(&saving_path, &test_wav()).unwrap();
        db.reconcile_audio_assets(&root, true).unwrap();
        let ready = db.get_transcriptions(10, 0).unwrap().remove(0);
        let ready_asset = ready.audio_asset.unwrap();
        assert_eq!(ready_asset.status, AudioAssetStatus::Ready);
        assert_eq!(
            ready_asset.path.as_deref(),
            Some(saving_path.to_str().unwrap())
        );
        assert!(ready_asset.byte_length.unwrap() > 0);

        std::fs::remove_file(&saving_path).unwrap();
        db.mark_audio_asset_missing(saving_asset_id).unwrap();
        let missing = db.get_audio_asset(saving_asset_id).unwrap().unwrap();
        assert_eq!(missing.status, AudioAssetStatus::Missing);
        assert_eq!(missing.error.as_deref(), Some("file_missing"));

        let failed_id = db
            .save_transcription("failed", None, "none", None, None, None)
            .unwrap();
        let (failed_asset_id, _) = db
            .begin_audio_asset("dictation", failed_id, "main")
            .unwrap();
        let failed_path = root.join("failed.wav");
        db.set_audio_asset_path(failed_asset_id, &failed_path)
            .unwrap();
        db.mark_audio_asset_failed(failed_asset_id, "write_failed")
            .unwrap();
        let failed = db
            .get_transcriptions(10, 0)
            .unwrap()
            .into_iter()
            .find(|row| row.id == failed_id)
            .unwrap();
        let failed_asset = failed.audio_asset.unwrap();
        assert_eq!(failed_asset.status, AudioAssetStatus::Failed);
        assert_eq!(failed_asset.path, None);
        assert_eq!(failed_asset.error.as_deref(), Some("write_failed"));

        let interrupted_id = db
            .save_transcription("interrupted", None, "none", None, None, None)
            .unwrap();
        let (interrupted_asset_id, _) = db
            .begin_audio_asset("dictation", interrupted_id, "main")
            .unwrap();
        let interrupted_path = root.join("interrupted.wav");
        db.set_audio_asset_path(interrupted_asset_id, &interrupted_path)
            .unwrap();
        let interrupted_temp = crate::audio::archive::temp_path(&interrupted_path);
        std::fs::write(&interrupted_temp, &test_wav()[..8]).unwrap();
        db.reconcile_audio_assets(&root, true).unwrap();
        let interrupted = db
            .get_transcriptions(10, 0)
            .unwrap()
            .into_iter()
            .find(|row| row.id == interrupted_id)
            .unwrap();
        let interrupted_asset = interrupted.audio_asset.unwrap();
        assert_eq!(interrupted_asset.status, AudioAssetStatus::Failed);
        assert_eq!(interrupted_asset.error.as_deref(), Some("interrupted"));
        assert!(!interrupted_path.exists());
        assert!(!interrupted_temp.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_audio_rows_reconcile_to_ready_or_missing() {
        let db = Database::new_in_memory().unwrap();
        let root = audio_test_root("legacy");
        let existing_path = root.join("legacy-existing.wav");
        let missing_path = root.join("legacy-missing.wav");
        crate::audio::archive::write_wav_atomically(&existing_path, &test_wav()).unwrap();

        let (existing_id, missing_id) = {
            let conn = db.conn.lock().unwrap();
            conn.execute("DELETE FROM audio_assets", []).unwrap();
            conn.execute_batch("PRAGMA user_version = 5;").unwrap();
            conn.execute(
                "INSERT INTO transcriptions (original_text, audio_path) VALUES (?1, ?2)",
                rusqlite::params!["legacy existing", existing_path.to_string_lossy()],
            )
            .unwrap();
            let existing_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO transcriptions (original_text, audio_path) VALUES (?1, ?2)",
                rusqlite::params!["legacy missing", missing_path.to_string_lossy()],
            )
            .unwrap();
            let missing_id = conn.last_insert_rowid();
            migrations::run(&conn).unwrap();
            (existing_id, missing_id)
        };

        db.reconcile_audio_assets(&root, true).unwrap();
        let rows = db.get_transcriptions(10, 0).unwrap();
        let existing = rows.iter().find(|row| row.id == existing_id).unwrap();
        assert_eq!(
            existing.audio_asset.as_ref().unwrap().status,
            AudioAssetStatus::Ready
        );
        let missing = rows.iter().find(|row| row.id == missing_id).unwrap();
        let missing_asset = missing.audio_asset.as_ref().unwrap();
        assert_eq!(missing_asset.status, AudioAssetStatus::Missing);
        assert_eq!(missing_asset.error.as_deref(), Some("file_missing"));
        assert_eq!(missing_asset.path, None);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn note_audio_segments_preserve_append_order() {
        let db = Database::new_in_memory().unwrap();
        let root = audio_test_root("note-order");
        let note_id = db.create_note().unwrap();
        let first_path = root.join("first.wav");
        let second_path = root.join("second.wav");
        let (_, first_sequence) = create_ready_asset(&db, "note", note_id, "main", &first_path);
        let (_, second_sequence) = create_ready_asset(&db, "note", note_id, "main", &second_path);

        assert_eq!(first_sequence, 0);
        assert_eq!(second_sequence, 1);
        let note = db.get_note(note_id).unwrap();
        assert_eq!(note.audio_segments.len(), 2);
        assert_eq!(note.audio_segments[0].sequence, 0);
        assert_eq!(note.audio_segments[1].sequence, 1);
        assert_eq!(
            note.audio_segments[0].path.as_deref(),
            Some(first_path.to_str().unwrap())
        );
        assert_eq!(
            note.audio_segments[1].path.as_deref(),
            Some(second_path.to_str().unwrap())
        );
        assert_eq!(
            note.audio_path.as_deref(),
            Some(second_path.to_str().unwrap())
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repeated_audio_asset_reads_do_not_create_sources_or_assets() {
        let db = Database::new_in_memory().unwrap();
        let root = audio_test_root("read-only-playback");
        let source_id = db
            .save_transcription("read-only", None, "none", None, None, None)
            .unwrap();
        let path = root.join("read-only.wav");
        let (asset_id, _) = create_ready_asset(&db, "dictation", source_id, "main", &path);
        let mut long_wav =
            vec![0_u8; (crate::commands::playback::MAX_RESPONSE_BYTES * 2 + 31) as usize];
        long_wav[0..4].copy_from_slice(b"RIFF");
        long_wav[8..12].copy_from_slice(b"WAVE");
        std::fs::write(&path, long_wav).unwrap();

        let counts = || {
            let conn = db.conn.lock().unwrap();
            let sources: i64 = conn
                .query_row("SELECT COUNT(*) FROM transcriptions", [], |row| row.get(0))
                .unwrap();
            let assets: i64 = conn
                .query_row("SELECT COUNT(*) FROM audio_assets", [], |row| row.get(0))
                .unwrap();
            (sources, assets)
        };
        let before = counts();

        for start in [
            0,
            crate::commands::playback::MAX_RESPONSE_BYTES,
            crate::commands::playback::MAX_RESPONSE_BYTES * 2,
        ] {
            let asset = db.get_audio_asset(asset_id).unwrap().unwrap();
            assert_eq!(asset.id, asset_id);
            assert_eq!(asset.owner_id, source_id);
            assert_eq!(asset.status, AudioAssetStatus::Ready);
            let request = tauri::http::Request::builder()
                .method(tauri::http::Method::GET)
                .header(tauri::http::header::RANGE, format!("bytes={start}-"))
                .uri(format!("/audio/{asset_id}"))
                .body(Vec::new())
                .unwrap();
            assert_eq!(
                crate::commands::playback::serve_file(request, &path).status(),
                tauri::http::StatusCode::PARTIAL_CONTENT
            );
        }

        assert_eq!(counts(), before);
        assert_eq!(db.get_transcriptions(10, 0).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_sources_removes_all_segments_and_preserves_outside_files() {
        let db = Database::new_in_memory().unwrap();
        let root = audio_test_root("delete-sources");
        let outside = root.parent().unwrap().join(format!(
            "{}-outside.wav",
            root.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&outside, b"unrelated").unwrap();

        let dictation_id = db
            .save_transcription("dictation", None, "none", None, None, None)
            .unwrap();
        let dictation_path = root.join("dictation.wav");
        create_ready_asset(&db, "dictation", dictation_id, "main", &dictation_path);

        let conversation_id = db.create_conversation(None).unwrap();
        let conversation_me = root.join("conversation-me.wav");
        let conversation_them = root.join("conversation-them.wav");
        create_ready_asset(&db, "conversation", conversation_id, "me", &conversation_me);
        create_ready_asset(
            &db,
            "conversation",
            conversation_id,
            "them",
            &conversation_them,
        );

        let note_id = db.create_note().unwrap();
        let note_first = root.join("note-first.wav");
        let note_missing = root.join("note-missing.wav");
        create_ready_asset(&db, "note", note_id, "main", &note_first);
        create_ready_asset(&db, "note", note_id, "main", &note_missing);
        std::fs::remove_file(&note_missing).unwrap();

        let (outside_asset_id, _) = db.begin_audio_asset("note", note_id, "main").unwrap();
        db.set_audio_asset_path(outside_asset_id, &outside).unwrap();
        db.mark_audio_asset_ready(outside_asset_id, 9, 1).unwrap();

        db.delete_transcription(dictation_id, &root).unwrap();
        db.delete_conversation(conversation_id, &root).unwrap();
        db.delete_note(note_id, &root).unwrap();

        assert!(!dictation_path.exists());
        assert!(!conversation_me.exists());
        assert!(!conversation_them.exists());
        assert!(!note_first.exists());
        assert!(outside.exists());

        // Missing files and repeated deletion are both harmless.
        db.delete_transcription(dictation_id, &root).unwrap();
        db.delete_conversation(conversation_id, &root).unwrap();
        db.delete_note(note_id, &root).unwrap();
        assert!(db.begin_audio_asset("note", note_id, "main").is_err());

        let conn = db.conn.lock().unwrap();
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM audio_assets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
        drop(conn);

        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(root);
    }
}
