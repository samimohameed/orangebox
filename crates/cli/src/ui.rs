//! Local web UI: `orangebox ui`.
//!
//! Serves the embedded React app on 127.0.0.1 and answers JSON API
//! requests through the same application services as the CLI commands.
//! While the UI is open, a background thread keeps recording, so viewing
//! and recording are one process.

use std::path::PathBuf;
use std::time::Duration;

use orangebox_application::services::{ArchiveService, RecorderService};
use orangebox_infrastructure::sqlite::SqliteArchive;
use orangebox_infrastructure::watcher::FsWatcher;
use tiny_http::{Header, Response, Server};

use crate::dto::{HitDto, StatusDto, SummaryDto, TranscriptDto};

// The React app (ui/ at the repo root), built by `npm run build` into a
// single self-contained file and embedded at compile time. The built file
// is committed so `cargo install` works without a JS toolchain.
const PAGE: &str = include_str!("../../../ui/dist/index.html");

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

/// Start the background recording thread: its own repository connection
/// (SQLite WAL supports concurrent writer + readers) and its own adapters.
fn spawn_recorder(db_path: PathBuf) {
    std::thread::spawn(move || {
        let repo = match SqliteArchive::open(&db_path) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("recorder thread: cannot open archive: {e}");
                return;
            }
        };
        let mut recorder = RecorderService::new(repo, crate::adapters());
        if let Err(e) = recorder.scan_all() {
            tracing::warn!("recorder backfill failed: {e}");
        }
        let watcher = match FsWatcher::watch(&recorder.watch_roots(), Duration::from_secs(2)) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("recorder thread: watcher failed: {e}");
                return;
            }
        };
        while let Some(paths) = watcher.next_changed_paths() {
            for path in paths {
                if let Err(e) = recorder.ingest_changed_path(&path) {
                    tracing::warn!(path = %path.display(), "ingest failed: {e}");
                }
            }
        }
    });
}

pub fn serve(db_path: PathBuf, port: u16) -> orangebox_application::Result<()> {
    let archive = ArchiveService::new(SqliteArchive::open(&db_path)?);
    spawn_recorder(db_path);

    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| orangebox_application::ArchiveError::Storage(format!("bind {addr}: {e}")))?;
    println!("Orangebox UI: http://{addr}  (recording in background · ctrl-c to stop)");

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
                match archive.timeline(limit) {
                    Ok(items) => {
                        let dto: Vec<SummaryDto> = items.iter().map(SummaryDto::from).collect();
                        json_response(serde_json::to_string(&dto).unwrap())
                    }
                    Err(e) => error_response(500, &e.to_string()),
                }
            }
            "/api/search" => {
                let q = query_param(&url, "q").unwrap_or_default();
                match archive.search(&q, 50) {
                    Ok(hits) => {
                        let dto: Vec<HitDto> = hits.iter().map(HitDto::from).collect();
                        json_response(serde_json::to_string(&dto).unwrap())
                    }
                    Err(e) => error_response(400, &e.to_string()),
                }
            }
            "/api/session" => {
                let id = query_param(&url, "id").unwrap_or_default();
                match archive.transcript(&id) {
                    Ok(transcript) => {
                        let dto = TranscriptDto::from(&transcript);
                        json_response(serde_json::to_string(&dto).unwrap())
                    }
                    Err(e) => error_response(404, &e.to_string()),
                }
            }
            "/api/export" => {
                let id = query_param(&url, "id").unwrap_or_default();
                match archive.export_markdown(&id) {
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
            "/api/status" => match archive.stats() {
                Ok(stats) => {
                    let dto = StatusDto::from(stats);
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
