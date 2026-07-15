//! SQLite implementation of the `ArchiveRepository` port.
//!
//! WAL mode for crash-safety, FTS5 for full-text search. This is Blackbox's
//! own database — tool storage is never written to.

use std::path::Path;

use blackbox_application::ports::{
    ArchiveRepository, ArchiveStats, IngestBatch, SearchHit, SessionSummary,
};
use blackbox_application::{ArchiveError, Result};
use blackbox_domain::{Message, Role, Session, ToolKind};
use rusqlite::{params, Connection};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id               TEXT PRIMARY KEY,
    tool             TEXT NOT NULL,
    project          TEXT,
    started_at_ms    INTEGER NOT NULL,
    last_activity_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id            TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL REFERENCES sessions(id),
    role          TEXT NOT NULL,
    content       TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    seq           INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_sessions_activity ON sessions(last_activity_ms DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    content='messages',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES ('delete', old.rowid, old.content);
END;
"#;

pub struct SqliteArchive {
    conn: Connection,
}

impl SqliteArchive {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ArchiveError::Storage(format!("create {}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path).map_err(storage_err)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(storage_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(storage_err)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(storage_err)?;
        conn.execute_batch(SCHEMA).map_err(storage_err)?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO NOTHING",
            params![SCHEMA_VERSION.to_string()],
        )
        .map_err(storage_err)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(storage_err)?;
        conn.execute_batch(SCHEMA).map_err(storage_err)?;
        Ok(Self { conn })
    }
}

impl ArchiveRepository for SqliteArchive {
    fn store(&mut self, batch: &IngestBatch) -> Result<usize> {
        let tx = self.conn.transaction().map_err(storage_err)?;
        for s in &batch.sessions {
            // Keep the earliest start and the latest activity across re-ingests.
            tx.execute(
                "INSERT INTO sessions (id, tool, project, started_at_ms, last_activity_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    project          = COALESCE(excluded.project, sessions.project),
                    started_at_ms    = MIN(sessions.started_at_ms, excluded.started_at_ms),
                    last_activity_ms = MAX(sessions.last_activity_ms, excluded.last_activity_ms)",
                params![s.id, s.tool.slug(), s.project, s.started_at_ms, s.last_activity_ms],
            )
            .map_err(storage_err)?;
        }
        let mut new_messages = 0usize;
        for m in &batch.messages {
            let inserted = tx
                .execute(
                    "INSERT INTO messages (id, session_id, role, content, created_at_ms, seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(id) DO NOTHING",
                    params![m.id, m.session_id, m.role.slug(), m.content, m.created_at_ms, m.seq],
                )
                .map_err(storage_err)?;
            new_messages += inserted;
        }
        tx.commit().map_err(storage_err)?;
        Ok(new_messages)
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // Quote the user's words so FTS5 operators in casual queries
        // ("don't", "a-b") can't produce syntax errors.
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");

        let mut stmt = self
            .conn
            .prepare(
                "SELECT m.id, m.session_id, m.role, m.content, m.created_at_ms, m.seq,
                        s.tool, s.project, s.started_at_ms, s.last_activity_ms,
                        snippet(messages_fts, 0, '>>', '<<', ' … ', 18)
                 FROM messages_fts f
                 JOIN messages m ON m.rowid = f.rowid
                 JOIN sessions s ON s.id = m.session_id
                 WHERE messages_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(storage_err)?;

        let rows = stmt
            .query_map(params![fts_query, limit as i64], |row| {
                let role: String = row.get(2)?;
                let tool: String = row.get(6)?;
                Ok(SearchHit {
                    message: Message {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        role: Role::from_slug(&role).unwrap_or(Role::Assistant),
                        content: row.get(3)?,
                        created_at_ms: row.get(4)?,
                        seq: row.get(5)?,
                    },
                    session: Session {
                        id: row.get(1)?,
                        tool: ToolKind::from_slug(&tool).unwrap_or(ToolKind::ClaudeCode),
                        project: row.get(7)?,
                        started_at_ms: row.get(8)?,
                        last_activity_ms: row.get(9)?,
                    },
                    snippet: row.get(10)?,
                })
            })
            .map_err(storage_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_err)?;
        Ok(rows)
    }

    fn timeline(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.tool, s.project, s.started_at_ms, s.last_activity_ms,
                        (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id),
                        (SELECT m.content FROM messages m
                          WHERE m.session_id = s.id AND m.role = 'user'
                          ORDER BY m.seq ASC LIMIT 1)
                 FROM sessions s
                 ORDER BY s.last_activity_ms DESC
                 LIMIT ?1",
            )
            .map_err(storage_err)?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let tool: String = row.get(1)?;
                Ok(SessionSummary {
                    session: Session {
                        id: row.get(0)?,
                        tool: ToolKind::from_slug(&tool).unwrap_or(ToolKind::ClaudeCode),
                        project: row.get(2)?,
                        started_at_ms: row.get(3)?,
                        last_activity_ms: row.get(4)?,
                    },
                    message_count: row.get(5)?,
                    first_user_message: row.get(6)?,
                })
            })
            .map_err(storage_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_err)?;
        Ok(rows)
    }

    fn stats(&self) -> Result<ArchiveStats> {
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .map_err(storage_err)?;
        let messages: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .map_err(storage_err)?;
        let mut stmt = self
            .conn
            .prepare("SELECT tool, COUNT(*) FROM sessions GROUP BY tool ORDER BY tool")
            .map_err(storage_err)?;
        let tools = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(storage_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_err)?;
        Ok(ArchiveStats { sessions, messages, tools })
    }
}

fn storage_err(e: rusqlite::Error) -> ArchiveError {
    ArchiveError::Storage(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_batch() -> IngestBatch {
        let session = Session {
            id: "claude-code:s1".into(),
            tool: ToolKind::ClaudeCode,
            project: Some("/tmp/proj".into()),
            started_at_ms: 1_000,
            last_activity_ms: 2_000,
        };
        let messages = vec![
            Message {
                id: "m1".into(),
                session_id: session.id.clone(),
                role: Role::User,
                content: "please fix the sidebar bug".into(),
                created_at_ms: 1_000,
                seq: 0,
            },
            Message {
                id: "m2".into(),
                session_id: session.id.clone(),
                role: Role::Assistant,
                content: "the sidebar bug is caused by a missing flex gap".into(),
                created_at_ms: 2_000,
                seq: 1,
            },
        ];
        IngestBatch { sessions: vec![session], messages }
    }

    #[test]
    fn store_is_idempotent_and_searchable() {
        let mut archive = SqliteArchive::open_in_memory().unwrap();
        assert_eq!(archive.store(&sample_batch()).unwrap(), 2);
        assert_eq!(archive.store(&sample_batch()).unwrap(), 0);

        let hits = archive.search("sidebar", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].snippet.contains(">>sidebar<<"));

        let timeline = archive.timeline(10).unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].message_count, 2);
        assert_eq!(
            timeline[0].first_user_message.as_deref(),
            Some("please fix the sidebar bug")
        );
    }

    #[test]
    fn search_survives_fts_operator_characters() {
        let mut archive = SqliteArchive::open_in_memory().unwrap();
        archive.store(&sample_batch()).unwrap();
        // None of these may return a syntax error.
        for q in ["sidebar AND", "\"unbalanced", "a-b*", "don't"] {
            archive.search(q, 5).unwrap();
        }
    }
}
