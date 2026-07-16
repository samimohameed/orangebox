//! Application services: the use cases, shaped as services with
//! constructor-injected dependencies.
//!
//! Each service receives its ports at construction (the composition root
//! decides the concrete types) and exposes the operations as methods —
//! the Rust equivalent of C# services taking interfaces in the
//! constructor. `RecorderService` writes (needs adapters + a mutable
//! repository); `ArchiveService` reads.

use std::path::{Path, PathBuf};

use orangebox_domain::{Message, Session};

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

/// Recording side: discovers tool storage through the injected adapters
/// and journals what they parse into the injected repository.
pub struct RecorderService<R: ArchiveRepository> {
    repo: R,
    adapters: Vec<Box<dyn ToolAdapter>>,
}

impl<R: ArchiveRepository> RecorderService<R> {
    pub fn new(repo: R, adapters: Vec<Box<dyn ToolAdapter>>) -> Self {
        Self { repo, adapters }
    }

    /// Every directory the daemon should watch, across all adapters.
    pub fn watch_roots(&self) -> Vec<PathBuf> {
        self.adapters.iter().flat_map(|a| a.watch_roots()).collect()
    }

    /// Full backfill: discover every source file of every adapter and
    /// ingest it.
    pub fn scan_all(&mut self) -> Result<Vec<IngestReport>> {
        let mut reports = Vec::new();
        for adapter in &self.adapters {
            for path in adapter.discover()? {
                match ingest_one(adapter.as_ref(), &path, &mut self.repo) {
                    Ok(Some(report)) => reports.push(report),
                    Ok(None) => {}
                    // One unreadable file must not abort the scan;
                    // record and go on.
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
    pub fn ingest_changed_path(&mut self, path: &Path) -> Result<Option<IngestReport>> {
        for adapter in &self.adapters {
            if adapter.matches(path) {
                return ingest_one(adapter.as_ref(), path, &mut self.repo);
            }
        }
        Ok(None)
    }
}

fn ingest_one<R: ArchiveRepository>(
    adapter: &dyn ToolAdapter,
    path: &Path,
    repo: &mut R,
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

/// Reading side: search, timelines, transcripts, and recovery exports.
pub struct ArchiveService<R: ArchiveRepository> {
    repo: R,
}

impl<R: ArchiveRepository> ArchiveService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ArchiveError::InvalidInput(
                "search query must not be empty".into(),
            ));
        }
        self.repo.search(query, limit)
    }

    pub fn timeline(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        self.repo.timeline(limit)
    }

    pub fn stats(&self) -> Result<ArchiveStats> {
        self.repo.stats()
    }

    /// Retention: drop sessions idle for more than `keep_days` days.
    /// Explicit and manual by design — an archive that silently deletes
    /// its own data would defeat the product.
    pub fn prune(&mut self, keep_days: u32, now_ms: i64) -> Result<(usize, usize)> {
        if keep_days == 0 {
            return Err(ArchiveError::InvalidInput(
                "--keep-days must be at least 1".into(),
            ));
        }
        let cutoff = now_ms - i64::from(keep_days) * 24 * 60 * 60 * 1000;
        self.repo.prune(cutoff)
    }

    /// A full session transcript, for viewing and recovery.
    pub fn transcript(&self, session_id: &str) -> Result<(Session, Vec<Message>)> {
        self.repo.session_with_messages(session_id)?.ok_or_else(|| {
            ArchiveError::InvalidInput(format!("no session matches \"{session_id}\""))
        })
    }

    /// Render a session as a Markdown recovery document: paste it into a
    /// fresh session of any AI tool to restore the lost context. This is
    /// the recovery path — sessions cannot be injected back into the
    /// tools' own proprietary histories.
    pub fn export_markdown(&self, session_id: &str) -> Result<String> {
        let (session, messages) = self.transcript(session_id)?;

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
            "> Recovered by Orangebox. To restore context, paste the transcript \
             below into a new session and ask the assistant to continue from it.\n\n---\n\n",
        );
        for m in &messages {
            let speaker = match m.role {
                orangebox_domain::Role::User => "You",
                orangebox_domain::Role::Assistant => "Assistant",
                orangebox_domain::Role::System => "System",
                orangebox_domain::Role::Tool => "Tool",
            };
            out.push_str(&format!("## {speaker}\n\n{}\n\n", m.content.trim()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use orangebox_domain::{Message, Role, Session, ToolKind};

    use super::*;
    use crate::ports::IngestBatch;

    /// In-memory fakes injected through the constructors — the services
    /// only ever see the ports, which is the point of the architecture.
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
        fn prune(&mut self, older_than_ms: i64) -> Result<(usize, usize)> {
            let before = self.stored_messages.len();
            self.stored_messages.retain(|m| m.created_at_ms >= older_than_ms);
            Ok((0, before - self.stored_messages.len()))
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
            vec![PathBuf::from("/fake")]
        }
        fn discover(&self) -> Result<Vec<PathBuf>> {
            Ok(vec![PathBuf::from("/fake/session.jsonl")])
        }
        fn matches(&self, path: &std::path::Path) -> bool {
            path.extension().is_some_and(|e| e == "jsonl")
        }
        fn ingest(&self, _path: &std::path::Path) -> Result<IngestBatch> {
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

    fn recorder() -> RecorderService<FakeRepo> {
        RecorderService::new(FakeRepo::default(), vec![Box::new(FakeAdapter)])
    }

    #[test]
    fn scan_ingests_and_second_scan_is_idempotent() {
        let mut service = recorder();
        let first = service.scan_all().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].new_messages, 1);

        let second = service.scan_all().unwrap();
        assert_eq!(second[0].new_messages, 0, "re-scan must not duplicate");
    }

    #[test]
    fn watch_roots_aggregate_adapters() {
        assert_eq!(recorder().watch_roots(), vec![PathBuf::from("/fake")]);
    }

    #[test]
    fn empty_search_query_is_rejected() {
        let service = ArchiveService::new(FakeRepo::default());
        assert!(service.search("  ", 10).is_err());
    }

    #[test]
    fn export_renders_markdown_recovery_doc() {
        let mut rec = recorder();
        rec.scan_all().unwrap();
        let RecorderService { repo, .. } = rec;

        let service = ArchiveService::new(repo);
        let md = service.export_markdown("claude-code:s1").unwrap();
        assert!(md.contains("# Recovered session"));
        assert!(md.contains("## You"));
        assert!(md.contains("hello"));

        assert!(service.export_markdown("nope").is_err());
    }
}
