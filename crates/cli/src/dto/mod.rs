//! Presentation-layer DTOs for the web UI's JSON API.
//!
//! These shapes belong to the HTTP boundary, not to the domain: they are
//! what the React app consumes (mirrored in `ui/src/types.ts`). Domain
//! entities convert in via `From`, so handlers never build DTOs by hand.

use blackbox_application::ports::{ArchiveStats, SearchHit, SessionSummary};
use blackbox_domain::{Message, Session};
use serde::Serialize;

#[derive(Serialize)]
pub struct SessionDto {
    pub id: String,
    pub tool: &'static str,
    pub project: Option<String>,
    pub started_at_ms: i64,
    pub last_activity_ms: i64,
}

impl From<&Session> for SessionDto {
    fn from(s: &Session) -> Self {
        Self {
            id: s.id.clone(),
            tool: s.tool.slug(),
            project: s.project.clone(),
            started_at_ms: s.started_at_ms,
            last_activity_ms: s.last_activity_ms,
        }
    }
}

#[derive(Serialize)]
pub struct SummaryDto {
    #[serde(flatten)]
    pub session: SessionDto,
    pub message_count: i64,
    pub title: Option<String>,
}

impl From<&SessionSummary> for SummaryDto {
    fn from(s: &SessionSummary) -> Self {
        Self {
            session: SessionDto::from(&s.session),
            message_count: s.message_count,
            title: s.first_user_message.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct MessageDto {
    pub role: &'static str,
    pub content: String,
    pub created_at_ms: i64,
}

impl From<&Message> for MessageDto {
    fn from(m: &Message) -> Self {
        Self {
            role: m.role.slug(),
            content: m.content.clone(),
            created_at_ms: m.created_at_ms,
        }
    }
}

#[derive(Serialize)]
pub struct HitDto {
    pub session: SessionDto,
    pub snippet: String,
    pub role: &'static str,
    pub created_at_ms: i64,
}

impl From<&SearchHit> for HitDto {
    fn from(h: &SearchHit) -> Self {
        Self {
            session: SessionDto::from(&h.session),
            snippet: h.snippet.clone(),
            role: h.message.role.slug(),
            created_at_ms: h.message.created_at_ms,
        }
    }
}

#[derive(Serialize)]
pub struct TranscriptDto {
    pub session: SessionDto,
    pub messages: Vec<MessageDto>,
}

impl From<&(Session, Vec<Message>)> for TranscriptDto {
    fn from((session, messages): &(Session, Vec<Message>)) -> Self {
        Self {
            session: SessionDto::from(session),
            messages: messages.iter().map(MessageDto::from).collect(),
        }
    }
}

#[derive(Serialize)]
pub struct StatusDto {
    pub sessions: i64,
    pub messages: i64,
    pub tools: Vec<(String, i64)>,
}

impl From<ArchiveStats> for StatusDto {
    fn from(s: ArchiveStats) -> Self {
        Self {
            sessions: s.sessions,
            messages: s.messages,
            tools: s.tools,
        }
    }
}
