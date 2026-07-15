//! Ports: interfaces the application defines and infrastructure implements.

use std::path::{Path, PathBuf};

use blackbox_domain::{Message, Session, ToolKind};

use crate::Result;

/// A batch of parsed data produced by one adapter ingestion pass.
#[derive(Debug, Default)]
pub struct IngestBatch {
    pub sessions: Vec<Session>,
    pub messages: Vec<Message>,
}

impl IngestBatch {
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.messages.is_empty()
    }
}

/// Reads one AI tool's native storage and translates it into domain entities.
///
/// Adapters are read-only over the tool's files and must be idempotent:
/// ingesting the same file twice yields entities with the same ids.
pub trait ToolAdapter: Send {
    fn tool(&self) -> ToolKind;

    /// Directories the daemon should watch for changes.
    fn watch_roots(&self) -> Vec<PathBuf>;

    /// Every currently existing source file, for a full backfill scan.
    fn discover(&self) -> Result<Vec<PathBuf>>;

    /// Whether a changed path belongs to this adapter.
    fn matches(&self, path: &Path) -> bool;

    /// Parse one source file into domain entities. Returns an empty batch
    /// for files that match but contain nothing ingestible.
    fn ingest(&self, path: &Path) -> Result<IngestBatch>;
}

/// A full-text search hit.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub message: Message,
    pub session: Session,
    /// Snippet of the matched content with the match highlighted.
    pub snippet: String,
}

/// Aggregate view of one session for timeline listings.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session: Session,
    pub message_count: i64,
    /// First user message, as a human-recognizable title.
    pub first_user_message: Option<String>,
}

/// Counts for `blackbox status`.
#[derive(Debug, Clone, Default)]
pub struct ArchiveStats {
    pub sessions: i64,
    pub messages: i64,
    pub tools: Vec<(String, i64)>,
}

/// The archive Blackbox owns: session/message storage plus search.
pub trait ArchiveRepository: Send {
    /// Insert or update sessions and messages. Duplicate message ids are
    /// ignored (idempotent ingestion). Returns the number of messages that
    /// were actually new.
    fn store(&mut self, batch: &IngestBatch) -> Result<usize>;

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>>;

    fn timeline(&self, limit: usize) -> Result<Vec<SessionSummary>>;

    fn stats(&self) -> Result<ArchiveStats>;
}
