//! Adapter for Claude Code session files.
//!
//! Claude Code journals each session as append-only JSONL at
//! `~/.claude/projects/<project-slug>/<session-uuid>.jsonl`. Lines carry a
//! `type` field; conversation lines (`user`/`assistant`) hold a `message`
//! whose `content` is either a plain string or an array of typed blocks
//! (`text`, `thinking`, `tool_use`, `tool_result`). See FORMATS.md.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use blackbox_application::ports::{IngestBatch, ToolAdapter};
use blackbox_application::{ArchiveError, Result};
use blackbox_domain::{Message, Role, Session, ToolKind};
use chrono::DateTime;
use serde::Deserialize;

pub struct ClaudeCodeAdapter {
    projects_dir: PathBuf,
}

impl ClaudeCodeAdapter {
    /// Standard location: `~/.claude/projects`.
    pub fn new_default() -> Option<Self> {
        let dir = dirs::home_dir()?.join(".claude").join("projects");
        Some(Self { projects_dir: dir })
    }

    pub fn with_projects_dir(projects_dir: PathBuf) -> Self {
        Self { projects_dir }
    }
}

/// One JSONL line. Unknown `type`s deserialize fine and are skipped —
/// a Claude Code update must never break ingestion.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    uuid: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<InnerMessage>,
}

#[derive(Deserialize)]
struct InnerMessage {
    role: Option<String>,
    content: Option<ContentValue>,
}

/// `content` is a string for user messages, an array of blocks for
/// assistant messages.
#[derive(Deserialize)]
#[serde(untagged)]
enum ContentValue {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    name: Option<String>,
}

impl ContentValue {
    /// Flatten to plain text: text blocks verbatim, tool calls as a marker,
    /// thinking/signatures dropped.
    fn flatten(&self) -> String {
        match self {
            ContentValue::Text(s) => s.clone(),
            ContentValue::Blocks(blocks) => {
                let mut parts = Vec::new();
                for b in blocks {
                    match b.kind.as_deref() {
                        Some("text") => {
                            if let Some(t) = &b.text {
                                if !t.is_empty() {
                                    parts.push(t.clone());
                                }
                            }
                        }
                        Some("tool_use") => {
                            if let Some(n) = &b.name {
                                parts.push(format!("[tool: {n}]"));
                            }
                        }
                        _ => {}
                    }
                }
                parts.join("\n")
            }
        }
    }
}

fn parse_ts_ms(ts: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

impl ToolAdapter for ClaudeCodeAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::ClaudeCode
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        vec![self.projects_dir.clone()]
    }

    fn discover(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let Ok(projects) = std::fs::read_dir(&self.projects_dir) else {
            // Claude Code not installed — nothing to record, not an error.
            return Ok(files);
        };
        for project in projects.flatten() {
            if !project.path().is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    files.push(path);
                }
            }
        }
        files.sort();
        Ok(files)
    }

    fn matches(&self, path: &Path) -> bool {
        path.starts_with(&self.projects_dir)
            && path.extension().is_some_and(|e| e == "jsonl")
    }

    fn ingest(&self, path: &Path) -> Result<IngestBatch> {
        let file = File::open(path).map_err(|e| ArchiveError::Adapter {
            tool: "claude-code",
            message: format!("open {}: {e}", path.display()),
        })?;
        let reader = BufReader::new(file);

        let mut batch = IngestBatch::default();
        let mut session: Option<Session> = None;
        let mut seq: i64 = 0;

        for line in reader.lines() {
            // A torn final line (crash mid-write) or an unparseable line is
            // skipped, never fatal: the rest of the file still ingests.
            let Ok(line) = line else { continue };
            let Ok(parsed) = serde_json::from_str::<Line>(&line) else { continue };

            let is_conversation = matches!(parsed.kind.as_deref(), Some("user") | Some("assistant"));
            if !is_conversation {
                continue;
            }
            let (Some(uuid), Some(native_session)) = (&parsed.uuid, &parsed.session_id) else {
                continue;
            };
            let Some(inner) = &parsed.message else { continue };
            let content = inner.content.as_ref().map(|c| c.flatten()).unwrap_or_default();
            if content.trim().is_empty() {
                continue;
            }

            let ts_ms = parsed.timestamp.as_deref().and_then(parse_ts_ms).unwrap_or(0);
            let session_id = format!("claude-code:{native_session}");

            let entry = session.get_or_insert_with(|| Session {
                id: session_id.clone(),
                tool: ToolKind::ClaudeCode,
                project: None,
                started_at_ms: ts_ms,
                last_activity_ms: ts_ms,
            });
            if entry.project.is_none() {
                entry.project = parsed.cwd.clone();
            }
            if ts_ms > 0 {
                if entry.started_at_ms == 0 || ts_ms < entry.started_at_ms {
                    entry.started_at_ms = ts_ms;
                }
                if ts_ms > entry.last_activity_ms {
                    entry.last_activity_ms = ts_ms;
                }
            }

            let role = match inner.role.as_deref() {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                Some("system") => Role::System,
                _ => Role::Tool,
            };

            batch.messages.push(Message {
                id: format!("claude-code:{uuid}"),
                session_id,
                role,
                content,
                created_at_ms: ts_ms,
                seq,
            });
            seq += 1;
        }

        if let Some(s) = session {
            batch.sessions.push(s);
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const FIXTURE: &str = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-14T19:58:14.168Z","sessionId":"s-1","content":"queued"}
{"parentUuid":null,"type":"user","message":{"role":"user","content":"fix the sidebar please"},"uuid":"u-1","timestamp":"2026-07-14T19:58:14.211Z","cwd":"/Users/x/proj","sessionId":"s-1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"secret","signature":"xxx"},{"type":"text","text":"On it. The sidebar needs a flex gap."},{"type":"tool_use","name":"Edit","input":{}}]},"uuid":"a-1","timestamp":"2026-07-14T19:58:20.000Z","sessionId":"s-1"}
not even json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]},"uuid":"a-2","timestamp":"2026-07-14T19:59:00.000Z","sessionId":"s-1"#;

    fn write_fixture(dir: &Path) -> PathBuf {
        let project = dir.join("-Users-x-proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("s-1.jsonl");
        let mut f = File::create(&path).unwrap();
        f.write_all(FIXTURE.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_real_format_and_survives_torn_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = ClaudeCodeAdapter::with_projects_dir(tmp.path().to_path_buf());
        let path = write_fixture(tmp.path());

        assert!(adapter.matches(&path));
        assert_eq!(adapter.discover().unwrap(), vec![path.clone()]);

        let batch = adapter.ingest(&path).unwrap();
        assert_eq!(batch.sessions.len(), 1);
        let s = &batch.sessions[0];
        assert_eq!(s.id, "claude-code:s-1");
        assert_eq!(s.project.as_deref(), Some("/Users/x/proj"));

        // Torn last line and garbage line are skipped; 2 clean messages remain.
        assert_eq!(batch.messages.len(), 2);
        assert_eq!(batch.messages[0].role, Role::User);
        assert_eq!(batch.messages[0].content, "fix the sidebar please");
        let assistant = &batch.messages[1];
        assert!(assistant.content.contains("flex gap"));
        assert!(assistant.content.contains("[tool: Edit]"));
        assert!(
            !assistant.content.contains("secret"),
            "thinking blocks must not be ingested"
        );
    }
}
