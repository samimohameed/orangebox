//! Adapter for Antigravity IDE trajectory summaries.
//!
//! Antigravity keeps a VS Code-lineage SQLite database at
//! `User/globalStorage/state.vscdb`. The key
//! `antigravityUnifiedStateSync.trajectorySummaries` holds base64-encoded
//! protobuf: repeated entries `{1: trajectory-uuid, 2: {1: base64(inner)}}`
//! where the inner message is `{1: title-or-text, 2: step-count,
//! 3/7/10: timestamps {1: secs, 2: nanos}, 4: conversation-uuid,
//! 9: {1: workspace file:// URI}}`. Field map in FORMATS.md.
//!
//! Only summaries exist locally — full trajectory content is server-side.
//! Recording the summaries continuously still preserves what the IDE
//! itself loses on a crash: which sessions existed, their titles, projects,
//! progress, and terminal snippets.

use std::path::{Path, PathBuf};

use blackbox_application::ports::{IngestBatch, ToolAdapter};
use blackbox_application::{ArchiveError, Result};
use blackbox_domain::{Message, Role, Session, ToolKind};
use rusqlite::Connection;

use crate::proto::{self, Value};

const TRAJECTORY_KEY: &str = "antigravityUnifiedStateSync.trajectorySummaries";

pub struct AntigravityAdapter {
    /// `.../Antigravity IDE/User` — parent of globalStorage and
    /// workspaceStorage.
    user_dir: PathBuf,
}

impl AntigravityAdapter {
    pub fn new_default() -> Option<Self> {
        let dir = dirs::home_dir()?
            .join("Library/Application Support/Antigravity IDE/User");
        Some(Self { user_dir: dir })
    }

    pub fn with_user_dir(user_dir: PathBuf) -> Self {
        Self { user_dir }
    }

    fn global_db(&self) -> PathBuf {
        self.user_dir.join("globalStorage").join("state.vscdb")
    }

    /// Copy the database (with its WAL/SHM sidecars) to a temp location and
    /// read the copy — never hold the live file open while the IDE runs.
    fn read_trajectories_blob(&self, db: &Path) -> Result<Option<Vec<u8>>> {
        // Unique per call: concurrent ingestions (parallel tests, multiple
        // databases) must never share a scratch directory.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("blackbox-agy-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&tmp).map_err(io_err)?;
        let db_copy = tmp.join("state.vscdb");
        std::fs::copy(db, &db_copy).map_err(io_err)?;
        for ext in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{ext}", db.display()));
            if sidecar.exists() {
                let _ = std::fs::copy(&sidecar, tmp.join(format!("state.vscdb{ext}")));
            }
        }

        let result = (|| -> Result<Option<Vec<u8>>> {
            // The copy is private, so opening it writable is safe and lets
            // SQLite replay the copied WAL.
            let conn = Connection::open(&db_copy)
                .map_err(|e| adapter_err(format!("open db copy: {e}")))?;
            let value: Option<String> = conn
                .query_row(
                    "SELECT value FROM ItemTable WHERE key = ?1",
                    [TRAJECTORY_KEY],
                    |row| row.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(adapter_err(format!("read key: {other}"))),
                })?;
            Ok(value.and_then(|v| b64_decode_forgiving(v.trim())))
        })();

        let _ = std::fs::remove_dir_all(&tmp);
        result
    }
}

/// Base64 without external crates: the payloads are standard-alphabet,
/// sometimes with padding stripped by the double encoding.
fn b64_decode_forgiving(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|&c| c != b'=' && c != b'\n' && c != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        let mut bits = 0u32;
        for &c in chunk {
            acc = (acc << 6) | val(c)?;
            bits += 6;
        }
        while bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

fn io_err(e: std::io::Error) -> ArchiveError {
    adapter_err(e.to_string())
}

fn adapter_err(message: String) -> ArchiveError {
    ArchiveError::Adapter { tool: "antigravity", message }
}

/// Parse one trajectory entry's inner summary into a session + message.
fn parse_summary(traj_id: &str, inner: &[u8]) -> Option<(Session, Message)> {
    let fields = proto::walk(inner)?;

    let title = proto::first(&fields, 1).and_then(|v| proto::as_str(v).map(str::to_string))?;
    if title.trim().is_empty() {
        return None;
    }
    let steps = proto::first(&fields, 2).and_then(proto::as_varint);

    let created = proto::timestamp_ms(&fields, 7);
    let updated = [proto::timestamp_ms(&fields, 3), proto::timestamp_ms(&fields, 10), created]
        .into_iter()
        .flatten()
        .max();
    let started_at_ms = created.or(updated).unwrap_or(0);
    let last_activity_ms = updated.unwrap_or(started_at_ms);

    let project = proto::first_message(&fields, 9)
        .and_then(|ws| proto::first(&ws, 1).and_then(|v| proto::as_str(v).map(str::to_string)))
        .map(|uri| uri.strip_prefix("file://").map(str::to_string).unwrap_or(uri));

    let session_id = format!("antigravity:{traj_id}");
    let mut content = title;
    if let Some(steps) = steps {
        content.push_str(&format!("\n[trajectory summary · {steps} steps recorded]"));
    }

    let session = Session {
        id: session_id.clone(),
        tool: ToolKind::Antigravity,
        project,
        started_at_ms,
        last_activity_ms,
    };
    let message = Message {
        id: format!("antigravity:{traj_id}:summary"),
        session_id,
        role: Role::Assistant,
        content,
        created_at_ms: started_at_ms,
        seq: 0,
    };
    Some((session, message))
}

impl ToolAdapter for AntigravityAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Antigravity
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        vec![self.user_dir.join("globalStorage")]
    }

    fn discover(&self) -> Result<Vec<PathBuf>> {
        let db = self.global_db();
        Ok(if db.exists() { vec![db] } else { vec![] })
    }

    fn matches(&self, path: &Path) -> bool {
        if !path.starts_with(&self.user_dir) {
            return false;
        }
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("state.vscdb"))
    }

    fn ingest(&self, path: &Path) -> Result<IngestBatch> {
        // A change to the WAL/SHM sidecar means the database changed:
        // always read via the main file.
        let db = if path.extension().is_some_and(|e| e == "vscdb") {
            path.to_path_buf()
        } else {
            self.global_db()
        };
        if !db.exists() {
            return Ok(IngestBatch::default());
        }

        let Some(blob) = self.read_trajectories_blob(&db)? else {
            return Ok(IngestBatch::default());
        };
        let Some(entries) = proto::walk(&blob) else {
            // Format changed on us: not fatal, just nothing understood.
            tracing::warn!("antigravity: trajectory blob no longer parses; update FORMATS.md");
            return Ok(IngestBatch::default());
        };

        let mut batch = IngestBatch::default();
        for entry in &entries {
            if entry.number != 1 {
                continue;
            }
            let payload = match &entry.value {
                Value::Bytes(b) => proto::walk(b),
                Value::Str(s) => proto::walk(s.as_bytes()),
                _ => None,
            };
            let Some(entry_fields) = payload else { continue };
            let Some(traj_id) =
                proto::first(&entry_fields, 1).and_then(|v| proto::as_str(v).map(str::to_string))
            else {
                continue;
            };
            // Field 2 wraps `{1: base64(inner summary)}`.
            let Some(wrapper) = proto::first_message(&entry_fields, 2) else { continue };
            let Some(inner_b64) = proto::first(&wrapper, 1).and_then(proto::as_str) else {
                continue;
            };
            let Some(inner) = b64_decode_forgiving(inner_b64) else { continue };
            if let Some((session, message)) = parse_summary(&traj_id, &inner) {
                batch.sessions.push(session);
                batch.messages.push(message);
            }
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_varint(mut v: u64) -> Vec<u8> {
        let mut out = vec![];
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out
    }

    fn field_str(number: u64, s: &str) -> Vec<u8> {
        let mut out = encode_varint(number << 3 | 2);
        out.extend(encode_varint(s.len() as u64));
        out.extend(s.as_bytes());
        out
    }

    fn field_bytes(number: u64, b: &[u8]) -> Vec<u8> {
        let mut out = encode_varint(number << 3 | 2);
        out.extend(encode_varint(b.len() as u64));
        out.extend(b);
        out
    }

    fn field_varint(number: u64, v: u64) -> Vec<u8> {
        let mut out = encode_varint(number << 3);
        out.extend(encode_varint(v));
        out
    }

    fn b64encode(data: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let mut acc = 0u32;
            for (i, &b) in chunk.iter().enumerate() {
                acc |= u32::from(b) << (16 - 8 * i);
            }
            for i in 0..=chunk.len() {
                out.push(ALPHABET[((acc >> (18 - 6 * i)) & 0x3f) as usize] as char);
            }
        }
        out
    }

    /// Build a blob shaped exactly like the observed format.
    fn fixture_blob() -> Vec<u8> {
        let mut inner = field_str(1, "Designing Requests Moderation Page");
        inner.extend(field_varint(2, 101));
        let mut ts = field_varint(1, 1_779_648_894);
        ts.extend(field_varint(2, 957_614_000));
        inner.extend(field_bytes(7, &ts));
        inner.extend(field_str(4, "13bc9c84-f2ea-4ae3-ac8c-a471d7902e7d"));
        let ws = field_str(1, "file:///Users/x/Projects");
        inner.extend(field_bytes(9, &ws));

        let wrapper = field_str(1, &b64encode(&inner));
        let mut entry = field_str(1, "42cc27c1-74ef-4b64-9265-7798960277f3");
        entry.extend(field_bytes(2, &wrapper));
        field_bytes(1, &entry)
    }

    fn fixture_db(dir: &Path) -> PathBuf {
        let global = dir.join("globalStorage");
        std::fs::create_dir_all(&global).unwrap();
        let db_path = global.join("state.vscdb");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params![TRAJECTORY_KEY, b64encode(&fixture_blob())],
        )
        .unwrap();
        db_path
    }

    #[test]
    fn base64_round_trips_without_padding() {
        for data in [&b"a"[..], b"ab", b"abc", b"abcd", b"hello world!"] {
            assert_eq!(b64_decode_forgiving(&b64encode(data)).unwrap(), data);
        }
    }

    #[test]
    fn ingests_trajectory_summaries_from_vscdb() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = AntigravityAdapter::with_user_dir(tmp.path().to_path_buf());
        let db = fixture_db(tmp.path());

        assert!(adapter.matches(&db));
        assert!(adapter.matches(&PathBuf::from(format!("{}-wal", db.display()))));

        let batch = adapter.ingest(&db).unwrap();
        assert_eq!(batch.sessions.len(), 1);
        let s = &batch.sessions[0];
        assert_eq!(s.id, "antigravity:42cc27c1-74ef-4b64-9265-7798960277f3");
        assert_eq!(s.tool, ToolKind::Antigravity);
        assert_eq!(s.project.as_deref(), Some("/Users/x/Projects"));
        assert_eq!(s.started_at_ms, 1_779_648_894_957);

        let m = &batch.messages[0];
        assert!(m.content.contains("Designing Requests Moderation Page"));
        assert!(m.content.contains("101 steps"));
    }

    #[test]
    fn corrupt_blob_is_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("globalStorage");
        std::fs::create_dir_all(&global).unwrap();
        let db_path = global.join("state.vscdb");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, 'not-base64-protobuf!!!')",
            [TRAJECTORY_KEY],
        )
        .unwrap();
        drop(conn);

        let adapter = AntigravityAdapter::with_user_dir(tmp.path().to_path_buf());
        let batch = adapter.ingest(&db_path).unwrap();
        assert!(batch.is_empty());
    }
}
