//! Adapter for Antigravity IDE agent trajectories.
//!
//! Two local sources (field maps and schema in FORMATS.md):
//!
//! 1. **Full conversations** at
//!    `~/.gemini/antigravity-ide/conversations/<trajectory-uuid>.db` —
//!    one SQLite database per trajectory with a `steps` table whose
//!    protobuf `step_payload` blobs hold the user's prompts, the model's
//!    text, tool activity, file contents, and terminal output.
//! 2. **Trajectory summaries** in the VS Code-lineage database at
//!    `User/globalStorage/state.vscdb`, key
//!    `antigravityUnifiedStateSync.trajectorySummaries`: base64 protobuf
//!    entries `{1: trajectory-uuid, 2: {1: base64(inner)}}`, inner
//!    `{1: title, 2: step-count, 3/7/10: timestamps, 9: {1: workspace}}`.
//!    Summaries supply titles/projects and cover trajectories whose
//!    conversation file uses the older `.pb` format we don't parse yet.
//!
//! Both merge into one session per trajectory id at the storage layer.

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
    /// `~/.gemini/antigravity-ide/conversations` — one SQLite db per
    /// trajectory.
    conversations_dir: PathBuf,
}

/// A scratch copy of a live SQLite database (plus WAL/SHM sidecars).
/// Reading the copy means never holding the tool's file open while the IDE
/// runs. The directory is removed on drop.
struct DbSnapshot {
    dir: PathBuf,
    pub db: PathBuf,
}

impl DbSnapshot {
    fn take(db: &Path) -> Result<Self> {
        // Unique per call: concurrent ingestions (parallel tests, multiple
        // databases) must never share a scratch directory.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("blackbox-agy-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(io_err)?;
        let copy = dir.join("snapshot.db");
        std::fs::copy(db, &copy).map_err(io_err)?;
        for ext in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{ext}", db.display()));
            if sidecar.exists() {
                let _ = std::fs::copy(&sidecar, dir.join(format!("snapshot.db{ext}")));
            }
        }
        Ok(Self { dir, db: copy })
    }
}

impl Drop for DbSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl AntigravityAdapter {
    pub fn new_default() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self {
            user_dir: home.join("Library/Application Support/Antigravity IDE/User"),
            conversations_dir: home.join(".gemini/antigravity-ide/conversations"),
        })
    }

    pub fn with_dirs(user_dir: PathBuf, conversations_dir: PathBuf) -> Self {
        Self { user_dir, conversations_dir }
    }

    fn global_db(&self) -> PathBuf {
        self.user_dir.join("globalStorage").join("state.vscdb")
    }

    fn read_trajectories_blob(&self, db: &Path) -> Result<Option<Vec<u8>>> {
        let snapshot = DbSnapshot::take(db)?;
        // The copy is private, so opening it writable is safe and lets
        // SQLite replay the copied WAL.
        let conn = Connection::open(&snapshot.db)
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

/// Observed step-type → role mapping (FORMATS.md). Unknown types default
/// to Tool so new step types still get archived and searched.
fn step_role(step_type: i64) -> Option<Role> {
    match step_type {
        14 => Some(Role::User),
        // Model text: planning/prose (15), proposals (5), task docs (23),
        // trajectory self-summaries (85).
        5 | 15 | 23 | 85 => Some(Role::Assistant),
        // Ephemeral system reminders and injected history: noise.
        90 | 98 | 99 => None,
        _ => Some(Role::Tool),
    }
}

/// Binary blobs whose bytes happen to be valid UTF-8 masquerade as text;
/// embedded protobuf tags show up as control characters. Real prose never
/// contains those.
fn is_clean_text(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
}

const MAX_CHUNK_CHARS: usize = 4_000;
const MAX_MESSAGE_CHARS: usize = 12_000;

/// Recursively harvest human-readable text out of a protobuf payload whose
/// exact schema we don't fully know: prefer descending into nested
/// messages; keep leaf strings that look like prose (length + whitespace),
/// which filters out uuids, paths, and enum-ish tokens.
fn harvest_text(buf: &[u8], depth: u8, out: &mut Vec<String>) {
    let Some(fields) = proto::walk(buf) else { return };
    for field in &fields {
        match &field.value {
            Value::Bytes(b) if depth < 10 => harvest_text(b, depth + 1, out),
            Value::Str(s) => {
                if depth < 10 {
                    if let Some(_nested) = proto::walk(s.as_bytes()) {
                        let before = out.len();
                        harvest_text(s.as_bytes(), depth + 1, out);
                        if out.len() > before {
                            continue;
                        }
                    }
                }
                let trimmed = s.trim();
                if trimmed.len() >= 30
                    && trimmed.contains(char::is_whitespace)
                    && is_clean_text(trimmed)
                {
                    let mut chunk = trimmed.to_string();
                    if chunk.chars().count() > MAX_CHUNK_CHARS {
                        chunk = chunk.chars().take(MAX_CHUNK_CHARS).collect::<String>() + " …";
                    }
                    // Substring dedupe: nested encodings repeat the same
                    // text at several depths.
                    if out.iter().any(|e| e.contains(chunk.as_str())) {
                        continue;
                    }
                    out.retain(|e| !chunk.contains(e.as_str()));
                    out.push(chunk);
                }
            }
            _ => {}
        }
    }
}

/// One step row → one message, when it carries anything readable.
fn parse_step(
    session_id: &str,
    traj_id: &str,
    idx: i64,
    step_type: i64,
    payload: &[u8],
) -> Option<Message> {
    let role = step_role(step_type)?;

    let mut chunks = Vec::new();
    harvest_text(payload, 0, &mut chunks);
    let mut content = chunks.join("\n\n");
    if content.chars().count() > MAX_MESSAGE_CHARS {
        content = content.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + " …";
    }
    if content.trim().is_empty() {
        return None;
    }

    // Payload field 5 wraps `{1: {1: secs, 2: nanos}}`.
    let created_at_ms = proto::walk(payload)
        .and_then(|fields| proto::first_message(&fields, 5))
        .and_then(|wrapper| proto::timestamp_ms(&wrapper, 1))
        .unwrap_or(0);

    Some(Message {
        id: format!("antigravity:{traj_id}:step:{idx}"),
        session_id: session_id.to_string(),
        role,
        content,
        created_at_ms,
        // Summary message occupies seq 0.
        seq: idx + 1,
    })
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

impl AntigravityAdapter {
    /// Ingest one full-conversation database from `conversations/`.
    fn ingest_conversation(&self, db: &Path) -> Result<IngestBatch> {
        let Some(traj_id) = db.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
            return Ok(IngestBatch::default());
        };
        let session_id = format!("antigravity:{traj_id}");

        let snapshot = DbSnapshot::take(db)?;
        let conn = Connection::open(&snapshot.db)
            .map_err(|e| adapter_err(format!("open conversation copy: {e}")))?;

        let mut stmt = conn
            .prepare("SELECT idx, step_type, step_payload FROM steps ORDER BY idx ASC")
            .map_err(|e| adapter_err(format!("steps table: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })
            .map_err(|e| adapter_err(format!("read steps: {e}")))?;

        let mut batch = IngestBatch::default();
        let mut min_ts = i64::MAX;
        let mut max_ts = 0i64;
        for row in rows.flatten() {
            let (idx, step_type, payload) = row;
            let Some(payload) = payload else { continue };
            if let Some(message) = parse_step(&session_id, &traj_id, idx, step_type, &payload) {
                if message.created_at_ms > 0 {
                    min_ts = min_ts.min(message.created_at_ms);
                    max_ts = max_ts.max(message.created_at_ms);
                }
                batch.messages.push(message);
            }
        }
        if batch.messages.is_empty() {
            return Ok(batch);
        }

        batch.sessions.push(Session {
            id: session_id,
            tool: ToolKind::Antigravity,
            // The summaries source supplies the workspace; COALESCE at the
            // storage layer merges it in.
            project: None,
            started_at_ms: if min_ts == i64::MAX { 0 } else { min_ts },
            last_activity_ms: max_ts,
        });
        Ok(batch)
    }

    fn is_conversation_db(&self, path: &Path) -> bool {
        path.starts_with(&self.conversations_dir)
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".db") || n.ends_with(".db-wal") || n.ends_with(".db-shm"))
    }
}

impl ToolAdapter for AntigravityAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Antigravity
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        vec![self.user_dir.join("globalStorage"), self.conversations_dir.clone()]
    }

    fn discover(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let db = self.global_db();
        if db.exists() {
            files.push(db);
        }
        if let Ok(entries) = std::fs::read_dir(&self.conversations_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "db") {
                    files.push(path);
                }
            }
        }
        files.sort();
        Ok(files)
    }

    fn matches(&self, path: &Path) -> bool {
        if self.is_conversation_db(path) {
            return true;
        }
        path.starts_with(&self.user_dir)
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("state.vscdb"))
    }

    fn ingest(&self, path: &Path) -> Result<IngestBatch> {
        if self.is_conversation_db(path) {
            // WAL/SHM sidecar changes route to their main database file.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let db = if let Some(stem) = name.strip_suffix("-wal").or(name.strip_suffix("-shm")) {
                path.with_file_name(stem)
            } else {
                path.to_path_buf()
            };
            if !db.exists() {
                return Ok(IngestBatch::default());
            }
            return self.ingest_conversation(&db);
        }

        // Otherwise: the summaries key in state.vscdb. A change to the
        // WAL/SHM sidecar means the database changed; read the main file.
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

    fn adapter_for(dir: &Path) -> AntigravityAdapter {
        AntigravityAdapter::with_dirs(dir.to_path_buf(), dir.join("conversations"))
    }

    #[test]
    fn ingests_trajectory_summaries_from_vscdb() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = adapter_for(tmp.path());
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

        let adapter = adapter_for(tmp.path());
        let batch = adapter.ingest(&db_path).unwrap();
        assert!(batch.is_empty());
    }

    /// step_payload wire format: {1: step_type, 5: {1: {1: secs, 2: nanos}},
    /// N: text...} — text harvested from any nested field.
    fn step_payload(step_type: u64, secs: u64, text: &str) -> Vec<u8> {
        let mut out = field_varint(1, step_type);
        let mut ts = field_varint(1, secs);
        ts.extend(field_varint(2, 0));
        let wrapper = field_bytes(1, &ts);
        out.extend(field_bytes(5, &wrapper));
        out.extend(field_str(9, text));
        out
    }

    fn conversation_fixture(dir: &Path) -> PathBuf {
        let conv = dir.join("conversations");
        std::fs::create_dir_all(&conv).unwrap();
        let db_path = conv.join("11111111-2222-3333-4444-555555555555.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER NOT NULL,
             step_payload BLOB)",
        )
        .unwrap();
        let rows: [(i64, u64, &str); 4] = [
            (0, 14, "please design the moderation page following the rules"),
            (1, 90, "EPHEMERAL system reminder that must never be archived"),
            (2, 15, "I will start by analyzing the existing layout components"),
            (3, 8, "import React from 'react'; // full file contents captured"),
        ];
        for (idx, step_type, text) in rows {
            conn.execute(
                "INSERT INTO steps (idx, step_type, step_payload) VALUES (?1, ?2, ?3)",
                rusqlite::params![idx, step_type as i64, step_payload(step_type, 1_779_648_000 + idx as u64, text)],
            )
            .unwrap();
        }
        db_path
    }

    #[test]
    fn ingests_full_conversation_steps_with_roles() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = adapter_for(tmp.path());
        let db = conversation_fixture(tmp.path());

        assert!(adapter.matches(&db));
        assert!(adapter.discover().unwrap().contains(&db));

        let batch = adapter.ingest(&db).unwrap();
        assert_eq!(batch.sessions.len(), 1);
        assert_eq!(batch.sessions[0].id, "antigravity:11111111-2222-3333-4444-555555555555");
        assert_eq!(batch.sessions[0].started_at_ms, 1_779_648_000_000);

        // System step (type 90) skipped; user, assistant, tool kept.
        assert_eq!(batch.messages.len(), 3);
        assert_eq!(batch.messages[0].role, Role::User);
        assert!(batch.messages[0].content.contains("moderation page"));
        assert_eq!(batch.messages[1].role, Role::Assistant);
        assert_eq!(batch.messages[2].role, Role::Tool);
        assert!(batch.messages.iter().all(|m| !m.content.contains("EPHEMERAL")));
    }

    #[test]
    fn wal_sidecar_changes_route_to_main_conversation_db() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = adapter_for(tmp.path());
        let db = conversation_fixture(tmp.path());

        let wal = PathBuf::from(format!("{}-wal", db.display()));
        assert!(adapter.matches(&wal));
        let batch = adapter.ingest(&wal).unwrap();
        assert_eq!(batch.messages.len(), 3);
    }
}
