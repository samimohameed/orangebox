//! Adapter for VS Code Copilot Chat session files.
//!
//! VS Code Copilot Chat persists session files as JSON at
//! `<workspaceStorage>/<hash>/chatSessions/<session-uuid>.json`.
//! Per-workspace path metadata is read from `<workspaceStorage>/<hash>/workspace.json`.
//! See FORMATS.md.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use orangebox_application::ports::{IngestBatch, ToolAdapter};
use orangebox_application::{ArchiveError, Result};
use orangebox_domain::{Message, Role, Session, ToolKind};
use serde::Deserialize;

pub struct CopilotAdapter {
    workspace_storage_dir: PathBuf,
}

impl CopilotAdapter {
    /// Standard location: `<config_dir>/Code/User/workspaceStorage`.
    pub fn new_default() -> Option<Self> {
        let dir = dirs::config_dir()?
            .join("Code")
            .join("User")
            .join("workspaceStorage");
        Some(Self {
            workspace_storage_dir: dir,
        })
    }

    pub fn with_workspace_storage_dir(workspace_storage_dir: PathBuf) -> Self {
        Self {
            workspace_storage_dir,
        }
    }
}

#[derive(Deserialize)]
struct CopilotSession {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "creationDate")]
    creation_date: Option<i64>,
    #[serde(rename = "lastMessageDate")]
    last_message_date: Option<i64>,
    requests: Option<Vec<CopilotRequest>>,
}

#[derive(Deserialize)]
struct CopilotRequest {
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    timestamp: Option<i64>,
    message: Option<CopilotUserMessage>,
    response: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct CopilotUserMessage {
    text: Option<String>,
    parts: Option<Vec<CopilotUserPart>>,
}

#[derive(Deserialize)]
struct CopilotUserPart {
    text: Option<String>,
}

fn extract_user_text(msg: &CopilotUserMessage) -> String {
    if let Some(t) = &msg.text {
        if !t.trim().is_empty() {
            return t.clone();
        }
    }
    if let Some(parts) = &msg.parts {
        let text_parts: Vec<String> = parts
            .iter()
            .filter_map(|p| p.text.clone())
            .filter(|t| !t.trim().is_empty())
            .collect();
        if !text_parts.is_empty() {
            return text_parts.join("\n");
        }
    }
    String::new()
}

fn extract_assistant_text(response_val: &serde_json::Value) -> String {
    let mut parts = Vec::new();

    let items: Vec<&serde_json::Value> = match response_val {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        obj @ serde_json::Value::Object(_) => vec![obj],
        _ => vec![],
    };

    for item in items {
        if let Some(kind) = item.get("kind").and_then(|k| k.as_str()) {
            if kind == "progressMessage" {
                continue;
            }
        }

        if let Some(val) = item.get("value") {
            if let Some(s) = val.as_str() {
                if !s.trim().is_empty() {
                    parts.push(s.to_string());
                    continue;
                }
            }
        }

        if let Some(content) = item.get("content") {
            if let Some(s) = content.as_str() {
                if !s.trim().is_empty() {
                    parts.push(s.to_string());
                    continue;
                }
            } else if let Some(val) = content.get("value").and_then(|v| v.as_str()) {
                if !val.trim().is_empty() {
                    parts.push(val.to_string());
                    continue;
                }
            }
        }
    }

    parts.join("\n")
}

fn parse_workspace_folder(workspace_json_path: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct WorkspaceMeta {
        folder: Option<String>,
        workspace: Option<String>,
    }

    let content = std::fs::read_to_string(workspace_json_path).ok()?;
    let parsed: WorkspaceMeta = serde_json::from_str(&content).ok()?;
    let uri = parsed.folder.or(parsed.workspace)?;
    let raw = uri.strip_prefix("file://")?;

    let decoded = percent_decode(raw);
    if cfg!(windows) && decoded.starts_with('/') && decoded.chars().nth(2) == Some(':') {
        Some(decoded[1..].to_string())
    } else {
        Some(decoded)
    }
}

fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(val) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
            {
                bytes.push(val);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

impl ToolAdapter for CopilotAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Copilot
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        vec![self.workspace_storage_dir.clone()]
    }

    fn discover(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.workspace_storage_dir) else {
            return Ok(files);
        };

        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let chat_dir = entry.path().join("chatSessions");
            if !chat_dir.is_dir() {
                continue;
            }
            let Ok(session_entries) = std::fs::read_dir(chat_dir) else {
                continue;
            };
            for s_entry in session_entries.flatten() {
                let path = s_entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    files.push(path);
                }
            }
        }

        files.sort();
        Ok(files)
    }

    fn matches(&self, path: &Path) -> bool {
        path.starts_with(&self.workspace_storage_dir)
            && path.extension().is_some_and(|e| e == "json")
            && path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == "chatSessions")
    }

    fn ingest(&self, path: &Path) -> Result<IngestBatch> {
        let file = File::open(path).map_err(|e| ArchiveError::Adapter {
            tool: "copilot",
            message: format!("open {}: {e}", path.display()),
        })?;
        let reader = BufReader::new(file);

        let parsed: CopilotSession = match serde_json::from_reader(reader) {
            Ok(p) => p,
            Err(_) => {
                // Torn or unparseable JSON file: skip cleanly as per Orangebox principles.
                return Ok(IngestBatch::default());
            }
        };

        let native_session = parsed.session_id.or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
        });
        let Some(native_session) = native_session else {
            return Ok(IngestBatch::default());
        };

        let session_id = format!("copilot:{native_session}");
        let requests = parsed.requests.unwrap_or_default();

        let mut project = None;
        if let Some(workspace_dir) = path.parent().and_then(|p| p.parent()) {
            let ws_json = workspace_dir.join("workspace.json");
            if ws_json.exists() {
                project = parse_workspace_folder(&ws_json);
            }
        }

        let mut started_at_ms = parsed.creation_date.unwrap_or(0);
        let mut last_activity_ms = parsed.last_message_date.unwrap_or(started_at_ms);

        let mut batch = IngestBatch::default();
        let mut seq: i64 = 0;

        for req in &requests {
            let req_ts = req.timestamp.unwrap_or(0);
            if req_ts > 0 {
                if started_at_ms == 0 || req_ts < started_at_ms {
                    started_at_ms = req_ts;
                }
                if req_ts > last_activity_ms {
                    last_activity_ms = req_ts;
                }
            }

            // User prompt message
            if let Some(user_msg) = &req.message {
                let user_text = extract_user_text(user_msg);
                if !user_text.trim().is_empty() {
                    let msg_id = req
                        .request_id
                        .as_ref()
                        .map(|id| format!("copilot:{id}"))
                        .unwrap_or_else(|| format!("{session_id}:u:{seq}"));

                    batch.messages.push(Message {
                        id: msg_id,
                        session_id: session_id.clone(),
                        role: Role::User,
                        content: user_text,
                        created_at_ms: req_ts,
                        seq,
                    });
                    seq += 1;
                }
            }

            // Assistant response message
            if let Some(resp_val) = &req.response {
                let assistant_text = extract_assistant_text(resp_val);
                if !assistant_text.trim().is_empty() {
                    let msg_id = req
                        .response_id
                        .as_ref()
                        .map(|id| format!("copilot:{id}"))
                        .unwrap_or_else(|| format!("{session_id}:a:{seq}"));

                    batch.messages.push(Message {
                        id: msg_id,
                        session_id: session_id.clone(),
                        role: Role::Assistant,
                        content: assistant_text,
                        created_at_ms: req_ts,
                        seq,
                    });
                    seq += 1;
                }
            }
        }

        if !batch.messages.is_empty() || started_at_ms > 0 {
            batch.sessions.push(Session {
                id: session_id,
                tool: ToolKind::Copilot,
                project,
                started_at_ms,
                last_activity_ms,
            });
        }

        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const FIXTURE_JSON: &str = r#"{
  "version": 3,
  "requesterUsername": "testuser",
  "sessionId": "s-copilot-101",
  "creationDate": 1757059900000,
  "lastMessageDate": 1757059950000,
  "requests": [
    {
      "requestId": "req-1",
      "timestamp": 1757059910000,
      "message": {
        "text": "How do I list files in Rust?"
      },
      "response": [
        {
          "kind": "progressMessage",
          "content": { "value": "Searching workspace..." }
        },
        {
          "value": "You can use `std::fs::read_dir` to read entries in a directory."
        }
      ],
      "responseId": "resp-1"
    }
  ]
}"#;

    const WORKSPACE_JSON: &str = r#"{
  "folder": "file:///d%3A/Projects/rust-app"
}"#;

    fn write_fixture(dir: &Path) -> PathBuf {
        let ws_dir = dir.join("hash123");
        let chat_dir = ws_dir.join("chatSessions");
        std::fs::create_dir_all(&chat_dir).unwrap();

        let ws_file = ws_dir.join("workspace.json");
        let mut f_ws = File::create(&ws_file).unwrap();
        f_ws.write_all(WORKSPACE_JSON.as_bytes()).unwrap();

        let session_file = chat_dir.join("s-copilot-101.json");
        let mut f_s = File::create(&session_file).unwrap();
        f_s.write_all(FIXTURE_JSON.as_bytes()).unwrap();

        session_file
    }

    #[test]
    fn parses_copilot_session_and_extracts_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = CopilotAdapter::with_workspace_storage_dir(tmp.path().to_path_buf());
        let session_file = write_fixture(tmp.path());

        assert!(adapter.matches(&session_file));
        assert_eq!(adapter.discover().unwrap(), vec![session_file.clone()]);

        let batch = adapter.ingest(&session_file).unwrap();
        assert_eq!(batch.sessions.len(), 1);
        let s = &batch.sessions[0];
        assert_eq!(s.id, "copilot:s-copilot-101");
        assert_eq!(s.tool, ToolKind::Copilot);

        if cfg!(windows) {
            assert_eq!(s.project.as_deref(), Some("d:/Projects/rust-app"));
        } else {
            assert_eq!(s.project.as_deref(), Some("/d:/Projects/rust-app"));
        }

        assert_eq!(batch.messages.len(), 2);

        let u_msg = &batch.messages[0];
        assert_eq!(u_msg.id, "copilot:req-1");
        assert_eq!(u_msg.role, Role::User);
        assert_eq!(u_msg.content, "How do I list files in Rust?");

        let a_msg = &batch.messages[1];
        assert_eq!(a_msg.id, "copilot:resp-1");
        assert_eq!(a_msg.role, Role::Assistant);
        assert!(a_msg.content.contains("std::fs::read_dir"));
        assert!(
            !a_msg.content.contains("Searching workspace"),
            "progress messages must be filtered out"
        );
    }

    #[test]
    fn survives_corrupt_json() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = CopilotAdapter::with_workspace_storage_dir(tmp.path().to_path_buf());
        let chat_dir = tmp.path().join("hash2").join("chatSessions");
        std::fs::create_dir_all(&chat_dir).unwrap();
        let corrupt_file = chat_dir.join("corrupt.json");
        std::fs::write(&corrupt_file, "{ invalid json").unwrap();

        assert!(adapter.matches(&corrupt_file));
        let batch = adapter.ingest(&corrupt_file).unwrap();
        assert!(batch.is_empty());
    }
}
