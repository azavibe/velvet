pub mod migrations;
pub mod word_count;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;
use tauri::Manager;

pub struct Database {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcription {
    pub id: i64,
    pub timestamp: String,
    pub original_text: String,
    pub processed_text: Option<String>,
    pub is_processed: bool,
    pub processing_method: String,
    pub agent_name: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
    pub word_count: Option<i64>,
    /// Local WAV file, if the recording was archived to disk. Set by a
    /// background write shortly after the row is created (see
    /// `update_transcription_audio_path`), so a just-saved row can briefly
    /// have this as `None`.
    pub audio_path: Option<String>,
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

/// Summary row for the Conversations history list — no utterances/suggestions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub title: Option<String>,
    pub persona_name: Option<String>,
    /// Local WAV files for each channel, if archived (see
    /// `update_conversation_audio_paths`). Either can be `None` if that
    /// channel never captured any audio during the call.
    pub audio_path_me: Option<String>,
    pub audio_path_them: Option<String>,
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

/// Initialize the database and store it in Tauri's managed state
pub fn init(app: &AppHandle) -> Result<()> {
    let db_path = get_db_path(app)?;
    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;

    // Required for `ON DELETE CASCADE` on conversation_utterances/conversation_suggestions
    // (v3 migration) to actually cascade — SQLite ignores FK constraints unless this
    // pragma is set per-connection.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    migrations::run(&conn)?;

    app.manage(Database {
        conn: Mutex::new(conn),
    });

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
    fn new_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrations::run(&conn)?;
        Ok(Database {
            conn: Mutex::new(conn),
        })
    }

    pub fn save_transcription(
        &self,
        original_text: &str,
        processed_text: Option<&str>,
        processing_method: &str,
        agent_name: Option<&str>,
        error: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<i64> {
        // Count words on the final user-visible text — processed_text when AI
        // enhancement is on, otherwise the raw transcription.
        let counted_text = processed_text.unwrap_or(original_text);
        let word_count = word_count::count_words(counted_text);

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcriptions
               (original_text, processed_text, is_processed, processing_method,
                agent_name, error, duration_ms, word_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                original_text,
                processed_text,
                processed_text.is_some(),
                processing_method,
                agent_name,
                error,
                duration_ms,
                word_count,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_transcriptions(&self, limit: u32, offset: u32) -> Result<Vec<Transcription>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, original_text, processed_text, is_processed, processing_method, agent_name, error, duration_ms, word_count, audio_path
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
                audio_path: row.get(10)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Set once a dictation's audio has been written to disk (the write
    /// happens in a background task after the row is already saved, so
    /// this is a follow-up UPDATE rather than part of the initial INSERT).
    pub fn update_transcription_audio_path(&self, id: i64, path: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE transcriptions SET audio_path = ?2 WHERE id = ?1",
            rusqlite::params![id, path],
        )?;
        Ok(())
    }

    /// Deletes the row and, best-effort, its archived audio file. A missing
    /// or already-deleted file is not an error — the row is the source of
    /// truth for whether the recording ever existed.
    pub fn delete_transcription(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let audio_path: Option<String> = conn
            .query_row("SELECT audio_path FROM transcriptions WHERE id = ?1", [id], |r| r.get(0))
            .ok();
        conn.execute("DELETE FROM transcriptions WHERE id = ?1", [id])?;
        if let Some(path) = audio_path.filter(|p| !p.is_empty()) {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    pub fn clear_transcriptions(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let paths: Vec<String> = {
            let mut stmt = conn.prepare("SELECT audio_path FROM transcriptions WHERE audio_path IS NOT NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        conn.execute("DELETE FROM transcriptions", [])?;
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
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
                     WHERE duration_ms IS NOT NULL
                       AND timestamp >= datetime('now', 'localtime', 'start of day', 'utc')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
                StatsPeriod::Week => conn.query_row(
                    "SELECT COALESCE(SUM(duration_ms), 0),
                            COALESCE(SUM(word_count), 0),
                            COUNT(*)
                     FROM transcriptions
                     WHERE duration_ms IS NOT NULL
                       AND timestamp >= datetime('now', '-7 days')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
                StatsPeriod::All => conn.query_row(
                    "SELECT COALESCE(SUM(duration_ms), 0),
                            COALESCE(SUM(word_count), 0),
                            COUNT(*)
                     FROM transcriptions
                     WHERE duration_ms IS NOT NULL",
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
    pub fn get_recent_utterances(&self, conversation_id: i64, since_ms: i64) -> Result<Vec<ConversationUtterance>> {
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
                audio_path_me: row.get(5)?,
                audio_path_them: row.get(6)?,
                snippet: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_conversation(&self, conversation_id: i64) -> Result<ConversationDetail> {
        let conn = self.conn.lock().unwrap();

        let conversation = conn.query_row(
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
                    audio_path_me: row.get(5)?,
                    audio_path_them: row.get(6)?,
                    snippet: row.get(7)?,
                })
            },
        )?;

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
    pub fn delete_conversation(&self, conversation_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let audio_paths: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT audio_path_me, audio_path_them FROM conversations WHERE id = ?1",
                rusqlite::params![conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

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

        if let Some((me, them)) = audio_paths {
            if let Some(path) = me.filter(|p| !p.is_empty()) {
                let _ = std::fs::remove_file(path);
            }
            if let Some(path) = them.filter(|p| !p.is_empty()) {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        db.insert_conversation_utterance(id, "me", 0, "hello").unwrap();
        db.insert_conversation_suggestion(id, 0, None, "say hi back").unwrap();

        db.delete_conversation(id).unwrap();

        let conn = db.conn.lock().unwrap();
        let conversations: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        let utterances: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_utterances", [], |r| r.get(0))
            .unwrap();
        let suggestions: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_suggestions", [], |r| r.get(0))
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
        db.insert_conversation_utterance(id, "me", 1000, "old one").unwrap();
        db.insert_conversation_utterance(id, "them", 5000, "recent them").unwrap();
        db.insert_conversation_utterance(id, "me", 5200, "recent me").unwrap();

        let recent = db.get_recent_utterances(id, 4000).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].text, "recent them");
        assert_eq!(recent[1].text, "recent me");
    }
}
