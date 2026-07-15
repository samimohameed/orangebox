//! Use cases: the operations the CLI (and later the daemon/extension)
//! invoke. Each one is a small orchestration over the ports.

use std::path::Path;

use crate::ports::{
    ArchiveRepository, ArchiveStats, SearchHit, SessionSummary, ToolAdapter,
};
use crate::{ArchiveError, Result};

/// Outcome of scanning one source file.
#[derive(Debug)]
pub struct IngestReport {
    pub tool: &'static str,
    pub path: String,
    pub new_messages: usize,
}

/// Full backfill: discover every source file of every adapter and ingest it.
pub fn scan_all(
    adapters: &[Box<dyn ToolAdapter>],
    repo: &mut dyn ArchiveRepository,
) -> Result<Vec<IngestReport>> {
    let mut reports = Vec::new();
    for adapter in adapters {
        for path in adapter.discover()? {
            match ingest_one(adapter.as_ref(), &path, repo) {
                Ok(Some(report)) => reports.push(report),
                Ok(None) => {}
                // One unreadable file must not abort the scan; record and go on.
                Err(err) => reports.push(IngestReport {
                    tool: adapter.tool().slug(),
                    path: format!("{} (error: {err})", path.display()),
                    new_messages: 0,
                }),
            }
        }
    }
    Ok(reports)
}

/// Ingest a single changed path with whichever adapter claims it.
pub fn ingest_changed_path(
    adapters: &[Box<dyn ToolAdapter>],
    path: &Path,
    repo: &mut dyn ArchiveRepository,
) -> Result<Option<IngestReport>> {
    for adapter in adapters {
        if adapter.matches(path) {
            return ingest_one(adapter.as_ref(), path, repo);
        }
    }
    Ok(None)
}

fn ingest_one(
    adapter: &dyn ToolAdapter,
    path: &Path,
    repo: &mut dyn ArchiveRepository,
) -> Result<Option<IngestReport>> {
    let batch = adapter.ingest(path)?;
    if batch.is_empty() {
        return Ok(None);
    }
    let new_messages = repo.store(&batch)?;
    Ok(Some(IngestReport {
        tool: adapter.tool().slug(),
        path: path.display().to_string(),
        new_messages,
    }))
}

pub fn search(
    repo: &dyn ArchiveRepository,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ArchiveError::InvalidInput(
            "search query must not be empty".into(),
        ));
    }
    repo.search(query, limit)
}

pub fn timeline(repo: &dyn ArchiveRepository, limit: usize) -> Result<Vec<SessionSummary>> {
    repo.timeline(limit)
}

pub fn stats(repo: &dyn ArchiveRepository) -> Result<ArchiveStats> {
    repo.stats()
}

/// A full session transcript, for viewing and recovery.
pub fn transcript(
    repo: &dyn ArchiveRepository,
    session_id: &str,
) -> Result<(blackbox_domain::Session, Vec<blackbox_domain::Message>)> {
    repo.session_with_messages(session_id)?.ok_or_else(|| {
        ArchiveError::InvalidInput(format!("no session matches \"{session_id}\""))
    })
}

/// Render a session as a Markdown recovery document: paste it into a fresh
/// session of any AI tool to restore the lost context, or keep it as an
/// archive export. This is the recovery path — sessions cannot be injected
/// back into the tools' own proprietary histories.
pub fn export_markdown(repo: &dyn ArchiveRepository, session_id: &str) -> Result<String> {
    let (session, messages) = transcript(repo, session_id)?;

    let mut out = String::new();
    out.push_str(&format!(
        "# Recovered session — {}\n\n",
        session.project.as_deref().unwrap_or("unknown project")
    ));
    out.push_str(&format!(
        "- **Tool:** {}\n- **Session id:** `{}`\n- **Messages recorded:** {}\n\n",
        session.tool.slug(),
        session.id,
        messages.len()
    ));
    out.push_str(
        "> Recovered by Blackbox. To restore context, paste the transcript \
         below into a new session and ask the assistant to continue from it.\n\n---\n\n",
    );
    for m in &messages {
        let speaker = match m.role {
            blackbox_domain::Role::User => "You",
            blackbox_domain::Role::Assistant => "Assistant",
            blackbox_domain::Role::System => "System",
            blackbox_domain::Role::Tool => "Tool",
        };
        out.push_str(&format!("## {speaker}\n\n{}\n\n", m.content.trim()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use blackbox_domain::{Message, Role, Session, ToolKind};

    use super::*;
    use crate::ports::IngestBatch;

    /// In-memory fakes proving the use cases only need the ports —
    /// the point of the architecture.
    #[derive(Default)]
    struct FakeRepo {
        stored_messages: Vec<Message>,
    }

    impl ArchiveRepository for FakeRepo {
        fn store(&mut self, batch: &IngestBatch) -> Result<usize> {
            let mut new = 0;
            for m in &batch.messages {
                if !self.stored_messages.iter().any(|e| e.id == m.id) {
                    self.stored_messages.push(m.clone());
                    new += 1;
                }
            }
            Ok(new)
        }
        fn search(&self, _q: &str, _l: usize) -> Result<Vec<SearchHit>> {
            Ok(vec![])
        }
        fn timeline(&self, _l: usize) -> Result<Vec<SessionSummary>> {
            Ok(vec![])
        }
        fn stats(&self) -> Result<ArchiveStats> {
            Ok(ArchiveStats::default())
        }
        fn session_with_messages(
            &self,
            session_id: &str,
        ) -> Result<Option<(Session, Vec<Message>)>> {
            let messages: Vec<Message> = self
                .stored_messages
                .iter()
                .filter(|m| m.session_id.starts_with(session_id))
                .cloned()
                .collect();
            if messages.is_empty() {
                return Ok(None);
            }
            let session = Session {
                id: messages[0].session_id.clone(),
                tool: ToolKind::ClaudeCode,
                project: Some("/tmp/proj".into()),
                started_at_ms: 0,
                last_activity_ms: 1,
            };
            Ok(Some((session, messages)))
        }
    }

    struct FakeAdapter;

    impl ToolAdapter for FakeAdapter {
        fn tool(&self) -> ToolKind {
            ToolKind::ClaudeCode
        }
        fn watch_roots(&self) -> Vec<PathBuf> {
            vec![]
        }
        fn discover(&self) -> Result<Vec<PathBuf>> {
            Ok(vec![PathBuf::from("/fake/session.jsonl")])
        }
        fn matches(&self, path: &Path) -> bool {
            path.extension().is_some_and(|e| e == "jsonl")
        }
        fn ingest(&self, _path: &Path) -> Result<IngestBatch> {
            let session = Session {
                id: "claude-code:s1".into(),
                tool: ToolKind::ClaudeCode,
                project: None,
                started_at_ms: 0,
                last_activity_ms: 1,
            };
            let message = Message {
                id: "m1".into(),
                session_id: session.id.clone(),
                role: Role::User,
                content: "hello".into(),
                created_at_ms: 0,
                seq: 0,
            };
            Ok(IngestBatch {
                sessions: vec![session],
                messages: vec![message],
            })
        }
    }

    #[test]
    fn scan_ingests_and_second_scan_is_idempotent() {
        let adapters: Vec<Box<dyn ToolAdapter>> = vec![Box::new(FakeAdapter)];
        let mut repo = FakeRepo::default();

        let first = scan_all(&adapters, &mut repo).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].new_messages, 1);

        let second = scan_all(&adapters, &mut repo).unwrap();
        assert_eq!(second[0].new_messages, 0, "re-scan must not duplicate");
    }

    #[test]
    fn empty_search_query_is_rejected() {
        let repo = FakeRepo::default();
        assert!(search(&repo, "  ", 10).is_err());
    }

    #[test]
    fn export_renders_markdown_recovery_doc() {
        let adapters: Vec<Box<dyn ToolAdapter>> = vec![Box::new(FakeAdapter)];
        let mut repo = FakeRepo::default();
        scan_all(&adapters, &mut repo).unwrap();

        let md = export_markdown(&repo, "claude-code:s1").unwrap();
        assert!(md.contains("# Recovered session"));
        assert!(md.contains("## You"));
        assert!(md.contains("hello"));

        assert!(export_markdown(&repo, "nope").is_err());
    }
}
