//! Domain layer: the entities of the Orangebox archive.
//!
//! This crate is dependency-free. Timestamps are unix epoch milliseconds
//! (`i64`) so the domain does not need a datetime library; formatting and
//! parsing happen at the edges.

/// The AI coding tool a session was recorded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    ClaudeCode,
    Antigravity,
    Cursor,
    Copilot,
}

impl ToolKind {
    /// Stable identifier used in storage and CLI output.
    pub fn slug(self) -> &'static str {
        match self {
            ToolKind::ClaudeCode => "claude-code",
            ToolKind::Antigravity => "antigravity",
            ToolKind::Cursor => "cursor",
            ToolKind::Copilot => "copilot",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "claude-code" => Some(ToolKind::ClaudeCode),
            "antigravity" => Some(ToolKind::Antigravity),
            "cursor" => Some(ToolKind::Cursor),
            "copilot" => Some(ToolKind::Copilot),
            _ => None,
        }
    }
}

/// Who authored a message within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

impl Role {
    pub fn slug(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            "system" => Some(Role::System),
            "tool" => Some(Role::Tool),
            _ => None,
        }
    }
}

/// A recorded conversation with an AI tool.
///
/// `id` is the tool's native session identifier, prefixed with the tool slug
/// by the adapter (e.g. `claude-code:4af2facf-...`) so ids never collide
/// across tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub tool: ToolKind,
    /// Project path or workspace the session belongs to, when known.
    pub project: Option<String>,
    pub started_at_ms: i64,
    pub last_activity_ms: i64,
}

/// One message inside a session.
///
/// `id` is the tool's native message id when it has one (Claude Code message
/// uuid), otherwise a deterministic id derived by the adapter. Ingestion is
/// idempotent: re-reading a file re-produces the same ids and duplicates are
/// dropped at the storage boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    /// Plain-text content, already flattened from the tool's native format.
    pub content: String,
    pub created_at_ms: i64,
    /// Order within the session as observed in the source file.
    pub seq: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_slugs_round_trip() {
        for tool in [
            ToolKind::ClaudeCode,
            ToolKind::Antigravity,
            ToolKind::Cursor,
            ToolKind::Copilot,
        ] {
            assert_eq!(ToolKind::from_slug(tool.slug()), Some(tool));
        }
    }

    #[test]
    fn role_slugs_round_trip() {
        for role in [Role::User, Role::Assistant, Role::System, Role::Tool] {
            assert_eq!(Role::from_slug(role.slug()), Some(role));
        }
    }
}
