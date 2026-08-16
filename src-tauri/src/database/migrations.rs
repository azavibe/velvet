use anyhow::Result;
use rusqlite::Connection;

pub fn run(conn: &Connection) -> Result<()> {
    // v1: initial schema. Created without a user_version bump originally —
    // so a "fresh" v1 DB still reads user_version=0. The v2 step below treats
    // any version <2 as needing the v2 columns, which is idempotent in
    // either direction.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcriptions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
            original_text TEXT NOT NULL,
            processed_text TEXT,
            is_processed BOOLEAN DEFAULT 0,
            processing_method TEXT DEFAULT 'none',
            agent_name TEXT,
            error TEXT
        );",
    )?;

    // v2: add duration_ms + word_count for the Statistics tab.
    //
    // The guard checks both `user_version` AND the actual column presence.
    // The column check is belt-and-braces for downgrade/restore/dev scenarios
    // where the columns may already exist while user_version says otherwise —
    // a bare `ALTER TABLE ADD COLUMN` would then fail with "duplicate column
    // name" and prevent the app from starting.
    let version: i64 =
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 2 {
        if !column_exists(conn, "transcriptions", "duration_ms")? {
            conn.execute(
                "ALTER TABLE transcriptions ADD COLUMN duration_ms INTEGER",
                [],
            )?;
        }
        if !column_exists(conn, "transcriptions", "word_count")? {
            conn.execute(
                "ALTER TABLE transcriptions ADD COLUMN word_count INTEGER",
                [],
            )?;
        }
        conn.execute_batch("PRAGMA user_version = 2;")?;
    }

    // v3: Conversations feature — live call capture (mic + system-audio
    // loopback), the merged two-channel transcript, and the suggestions
    // generated from it. `conversations` is the parent row for a single
    // call; `conversation_utterances` holds one row per transcribed VAD
    // chunk on either channel, ordered for display by `started_at_ms`
    // (wall-clock, not per-channel sequence — the two channels transcribe
    // independently and can complete out of order); `conversation_suggestions`
    // records what the assistant proposed and when.
    if version < 3 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                ended_at DATETIME,
                title TEXT,
                persona_name TEXT
            );
            CREATE TABLE IF NOT EXISTS conversation_utterances (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                channel TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                text TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_utterances_conv
                ON conversation_utterances(conversation_id, started_at_ms);
            CREATE TABLE IF NOT EXISTS conversation_suggestions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                created_at_ms INTEGER NOT NULL,
                persona_name TEXT,
                text TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conversation_suggestions_conv
                ON conversation_suggestions(conversation_id, created_at_ms);",
        )?;
        conn.execute_batch("PRAGMA user_version = 3;")?;
    }

    Ok(())
}

fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    // `PRAGMA table_info(...)` returns rows of (cid, name, type, notnull, dflt, pk).
    // Table name is a Rust string literal (no user input), so format! is safe.
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == col {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migration_creates_table() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM transcriptions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_migration_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        run(&conn).unwrap(); // Should not fail on second run
    }

    #[test]
    fn v2_adds_duration_and_word_count_columns() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        // Both columns should exist and accept inserts.
        conn.execute(
            "INSERT INTO transcriptions (original_text, duration_ms, word_count) VALUES ('hi', 1234, 1)",
            [],
        )
        .unwrap();

        let (dur, wc): (i64, i64) = conn
            .query_row(
                "SELECT duration_ms, word_count FROM transcriptions WHERE id = last_insert_rowid()",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(dur, 1234);
        assert_eq!(wc, 1);
    }

    #[test]
    fn full_run_bumps_user_version_to_3() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(v, 3);
    }

    #[test]
    fn second_run_does_not_double_add_columns() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        // Second call would fail with "duplicate column name" if v2 weren't guarded.
        run(&conn).unwrap();
    }

    #[test]
    fn v2_skips_existing_columns_when_user_version_is_stale() {
        // Simulate a downgrade/dev DB: v1 schema + columns already present,
        // but user_version still 0. Without the per-column guard this would
        // fail with "duplicate column name".
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transcriptions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                original_text TEXT NOT NULL,
                duration_ms INTEGER,
                word_count INTEGER
            );",
        )
        .unwrap();
        // user_version is still 0 here.
        run(&conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(v, 3);
    }

    #[test]
    fn v3_creates_conversation_tables() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();

        conn.execute(
            "INSERT INTO conversations (title, persona_name) VALUES ('Standup', 'Sales')",
            [],
        )
        .unwrap();
        let conversation_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO conversation_utterances (conversation_id, channel, started_at_ms, text)
             VALUES (?1, 'me', 1000, 'hello')",
            rusqlite::params![conversation_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_suggestions (conversation_id, created_at_ms, persona_name, text)
             VALUES (?1, 2000, 'Sales', 'try mentioning the discount')",
            rusqlite::params![conversation_id],
        )
        .unwrap();

        let utterance_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_utterances WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(utterance_count, 1);
    }

    #[test]
    fn v3_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        // Second call would fail with "table already exists" if v3 weren't guarded.
        run(&conn).unwrap();
    }
}
