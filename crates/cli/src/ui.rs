//! Local web UI: `blackbox ui`.
//!
//! Serves an embedded single-page app on 127.0.0.1 and answers JSON API
//! requests by calling the same application use cases as the CLI commands.
//! While the UI is open, a background thread keeps watching tool storage,
//! so viewing and recording are one process.

use std::path::PathBuf;
use std::time::Duration;

use blackbox_application::usecases;
use blackbox_infrastructure::sqlite::SqliteArchive;
use blackbox_infrastructure::watcher::FsWatcher;
use serde::Serialize;
use tiny_http::{Header, Response, Server};

const PAGE: &str = include_str!("ui.html");

#[derive(Serialize)]
struct SessionDto {
    id: String,
    tool: &'static str,
    project: Option<String>,
    started_at_ms: i64,
    last_activity_ms: i64,
}

#[derive(Serialize)]
struct SummaryDto {
    #[serde(flatten)]
    session: SessionDto,
    message_count: i64,
    title: Option<String>,
}

#[derive(Serialize)]
struct MessageDto {
    role: &'static str,
    content: String,
    created_at_ms: i64,
}

#[derive(Serialize)]
struct HitDto {
    session: SessionDto,
    snippet: String,
    role: &'static str,
    created_at_ms: i64,
}

#[derive(Serialize)]
struct TranscriptDto {
    session: SessionDto,
    messages: Vec<MessageDto>,
}

#[derive(Serialize)]
struct StatusDto {
    sessions: i64,
    messages: i64,
    tools: Vec<(String, i64)>,
}

fn session_dto(s: &blackbox_domain::Session) -> SessionDto {
    SessionDto {
        id: s.id.clone(),
        tool: s.tool.slug(),
        project: s.project.clone(),
        started_at_ms: s.started_at_ms,
        last_activity_ms: s.last_activity_ms,
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3).and_then(|h| {
                    std::str::from_utf8(h).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
                });
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == name {
            return Some(percent_decode(v));
        }
    }
    None
}

fn json_response(body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body).with_header(
        Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap(),
    )
}

fn error_response(status: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(format!("{{\"error\":{}}}", serde_json::to_string(message).unwrap()))
        .with_status_code(status)
}

/// Start the background recording thread: its own archive connection
/// (SQLite WAL supports concurrent writer + readers) and its own adapters.
fn spawn_recorder(db_path: PathBuf) {
    std::thread::spawn(move || {
        let adapters = crate::adapters();
        let mut archive = match SqliteArchive::open(&db_path) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("recorder thread: cannot open archive: {e}");
                return;
            }
        };
        if let Err(e) = usecases::scan_all(&adapters, &mut archive) {
            tracing::warn!("recorder backfill failed: {e}");
        }
        let roots: Vec<PathBuf> = adapters.iter().flat_map(|a| a.watch_roots()).collect();
        let watcher = match FsWatcher::watch(&roots, Duration::from_secs(2)) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("recorder thread: watcher failed: {e}");
                return;
            }
        };
        while let Some(paths) = watcher.next_changed_paths() {
            for path in paths {
                if let Err(e) = usecases::ingest_changed_path(&adapters, &path, &mut archive) {
                    tracing::warn!(path = %path.display(), "ingest failed: {e}");
                }
            }
        }
    });
}

pub fn serve(db_path: PathBuf, port: u16) -> blackbox_application::Result<()> {
    let archive = SqliteArchive::open(&db_path)?;
    spawn_recorder(db_path);

    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| blackbox_application::ArchiveError::Storage(format!("bind {addr}: {e}")))?;
    println!("Blackbox UI: http://{addr}  (recording in background · ctrl-c to stop)");

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("/");

        let response = match path {
            "/" => Response::from_string(PAGE).with_header(
                Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
            ),
            "/api/timeline" => {
                let limit = query_param(&url, "n")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(100);
                match usecases::timeline(&archive, limit) {
                    Ok(items) => {
                        let dto: Vec<SummaryDto> = items
                            .iter()
                            .map(|s| SummaryDto {
                                session: session_dto(&s.session),
                                message_count: s.message_count,
                                title: s.first_user_message.clone(),
                            })
                            .collect();
                        json_response(serde_json::to_string(&dto).unwrap())
                    }
                    Err(e) => error_response(500, &e.to_string()),
                }
            }
            "/api/search" => {
                let q = query_param(&url, "q").unwrap_or_default();
                match usecases::search(&archive, &q, 50) {
                    Ok(hits) => {
                        let dto: Vec<HitDto> = hits
                            .iter()
                            .map(|h| HitDto {
                                session: session_dto(&h.session),
                                snippet: h.snippet.clone(),
                                role: h.message.role.slug(),
                                created_at_ms: h.message.created_at_ms,
                            })
                            .collect();
                        json_response(serde_json::to_string(&dto).unwrap())
                    }
                    Err(e) => error_response(400, &e.to_string()),
                }
            }
            "/api/session" => {
                let id = query_param(&url, "id").unwrap_or_default();
                match usecases::transcript(&archive, &id) {
                    Ok((session, messages)) => {
                        let dto = TranscriptDto {
                            session: session_dto(&session),
                            messages: messages
                                .iter()
                                .map(|m| MessageDto {
                                    role: m.role.slug(),
                                    content: m.content.clone(),
                                    created_at_ms: m.created_at_ms,
                                })
                                .collect(),
                        };
                        json_response(serde_json::to_string(&dto).unwrap())
                    }
                    Err(e) => error_response(404, &e.to_string()),
                }
            }
            "/api/export" => {
                let id = query_param(&url, "id").unwrap_or_default();
                match usecases::export_markdown(&archive, &id) {
                    Ok(md) => Response::from_string(md)
                        .with_header(
                            Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"text/markdown; charset=utf-8"[..],
                            )
                            .unwrap(),
                        )
                        .with_header(
                            Header::from_bytes(
                                &b"Content-Disposition"[..],
                                &b"attachment; filename=\"recovered-session.md\""[..],
                            )
                            .unwrap(),
                        ),
                    Err(e) => error_response(404, &e.to_string()),
                }
            }
            "/api/status" => match usecases::stats(&archive) {
                Ok(s) => {
                    let dto = StatusDto {
                        sessions: s.sessions,
                        messages: s.messages,
                        tools: s.tools,
                    };
                    json_response(serde_json::to_string(&dto).unwrap())
                }
                Err(e) => error_response(500, &e.to_string()),
            },
            _ => error_response(404, "not found"),
        };

        let _ = request.respond(response);
    }
    Ok(())
}
