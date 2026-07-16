//! Blackbox CLI — composition root.
//!
//! This binary is the only place where concrete infrastructure (SQLite,
//! adapters, watcher) is constructed and injected into the application
//! services.

mod dto;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use blackbox_application::ports::ToolAdapter;
use blackbox_application::services::{ArchiveService, RecorderService};
use blackbox_infrastructure::adapters::antigravity::AntigravityAdapter;
use blackbox_infrastructure::adapters::claude_code::ClaudeCodeAdapter;
use blackbox_infrastructure::sqlite::SqliteArchive;
use blackbox_infrastructure::watcher::FsWatcher;
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "blackbox", version, about = "Flight recorder for AI coding sessions")]
struct Cli {
    /// Path to the archive database (default: platform data dir).
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Backfill: ingest every existing session file from every tool.
    Scan,
    /// Watch tool storage and ingest changes as they happen (foreground).
    Watch,
    /// Full-text search across every recorded conversation.
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Recent sessions, newest first.
    Timeline {
        #[arg(short = 'n', long, default_value_t = 15)]
        limit: usize,
    },
    /// Archive statistics.
    Status,
    /// Print one session's full transcript.
    Show { session_id: String },
    /// Export a session as a Markdown recovery document.
    Export {
        session_id: String,
        /// Write to a file instead of stdout.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Open the local web UI (keeps recording while open).
    Ui {
        #[arg(short, long, default_value_t = 7171)]
        port: u16,
    },
}

/// Every tool adapter available on this machine, ready for injection.
pub(crate) fn adapters() -> Vec<Box<dyn ToolAdapter>> {
    let mut list: Vec<Box<dyn ToolAdapter>> = Vec::new();
    if let Some(claude) = ClaudeCodeAdapter::new_default() {
        list.push(Box::new(claude));
    }
    if let Some(antigravity) = AntigravityAdapter::new_default() {
        list.push(Box::new(antigravity));
    }
    // Phase 4: Cursor, Copilot.
    list
}

fn fmt_ts(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown time".into())
}

fn truncate(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn short_project(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "blackbox=info,warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(blackbox_infrastructure::default_db_path);

    let repo = match SqliteArchive::open(&db_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: cannot open archive at {}: {e}", db_path.display());
            std::process::exit(1);
        }
    };

    let result = match cli.command {
        Command::Scan => cmd_scan(RecorderService::new(repo, adapters())),
        Command::Watch => cmd_watch(RecorderService::new(repo, adapters())),
        Command::Search { query, limit } => {
            cmd_search(&ArchiveService::new(repo), &query.join(" "), limit)
        }
        Command::Timeline { limit } => cmd_timeline(&ArchiveService::new(repo), limit),
        Command::Status => cmd_status(&ArchiveService::new(repo), &db_path),
        Command::Show { session_id } => cmd_show(&ArchiveService::new(repo), &session_id),
        Command::Export { session_id, out } => {
            cmd_export(&ArchiveService::new(repo), &session_id, out)
        }
        Command::Ui { port } => {
            drop(repo); // the UI opens its own connections
            ui::serve(db_path, port)
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn cmd_scan(mut recorder: RecorderService<SqliteArchive>) -> blackbox_application::Result<()> {
    let reports = recorder.scan_all()?;
    let mut total_new = 0usize;
    for r in &reports {
        if r.new_messages > 0 {
            println!("  {} · {} new · {}", r.tool, r.new_messages, r.path);
        }
        total_new += r.new_messages;
    }
    println!(
        "scan complete: {} file(s) inspected, {} new message(s) archived",
        reports.len(),
        total_new
    );
    Ok(())
}

fn cmd_watch(mut recorder: RecorderService<SqliteArchive>) -> blackbox_application::Result<()> {
    // Catch up on anything written while we weren't running, then tail.
    let reports = recorder.scan_all()?;
    let backfilled: usize = reports.iter().map(|r| r.new_messages).sum();
    println!("backfill: {backfilled} new message(s); watching for changes (ctrl-c to stop)");

    let watcher = FsWatcher::watch(&recorder.watch_roots(), Duration::from_secs(2))?;

    while let Some(paths) = watcher.next_changed_paths() {
        for path in paths {
            match recorder.ingest_changed_path(&path) {
                Ok(Some(report)) if report.new_messages > 0 => {
                    println!(
                        "[{}] {} · {} new message(s)",
                        Local::now().format("%H:%M:%S"),
                        report.tool,
                        report.new_messages
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(path = %path.display(), "ingest failed: {e}"),
            }
        }
    }
    Ok(())
}

fn cmd_search(
    archive: &ArchiveService<SqliteArchive>,
    query: &str,
    limit: usize,
) -> blackbox_application::Result<()> {
    let hits = archive.search(query, limit)?;
    if hits.is_empty() {
        println!("no matches for \"{query}\"");
        return Ok(());
    }
    for hit in &hits {
        let project = hit
            .session
            .project
            .as_deref()
            .map(short_project)
            .unwrap_or_else(|| "unknown project".into());
        println!(
            "{} · {} · {} · {}",
            fmt_ts(hit.message.created_at_ms),
            hit.session.tool.slug(),
            project,
            hit.message.role.slug(),
        );
        println!("    {}", truncate(&hit.snippet, 200));
    }
    println!("\n{} match(es)", hits.len());
    Ok(())
}

fn cmd_timeline(
    archive: &ArchiveService<SqliteArchive>,
    limit: usize,
) -> blackbox_application::Result<()> {
    let sessions = archive.timeline(limit)?;
    if sessions.is_empty() {
        println!("archive is empty — run `blackbox scan` first");
        return Ok(());
    }
    for s in &sessions {
        let project = s
            .session
            .project
            .as_deref()
            .map(short_project)
            .unwrap_or_else(|| "unknown project".into());
        let title = s
            .first_user_message
            .as_deref()
            .map(|m| truncate(m, 90))
            .unwrap_or_else(|| "(no user message)".into());
        println!(
            "{} · {} · {} · {} msg(s)",
            fmt_ts(s.session.last_activity_ms),
            s.session.tool.slug(),
            project,
            s.message_count
        );
        println!("    {title}");
    }
    Ok(())
}

fn cmd_status(
    archive: &ArchiveService<SqliteArchive>,
    db_path: &PathBuf,
) -> blackbox_application::Result<()> {
    let stats = archive.stats()?;
    println!("archive: {}", db_path.display());
    println!("sessions: {}", stats.sessions);
    println!("messages: {}", stats.messages);
    for (tool, count) in &stats.tools {
        println!("  {tool}: {count} session(s)");
    }
    Ok(())
}

fn cmd_show(
    archive: &ArchiveService<SqliteArchive>,
    session_id: &str,
) -> blackbox_application::Result<()> {
    let (session, messages) = archive.transcript(session_id)?;
    println!(
        "{} · {} · {} message(s)\n",
        session.tool.slug(),
        session.project.as_deref().unwrap_or("unknown project"),
        messages.len()
    );
    for m in &messages {
        println!("── {} · {}", m.role.slug(), fmt_ts(m.created_at_ms));
        println!("{}\n", m.content.trim());
    }
    Ok(())
}

fn cmd_export(
    archive: &ArchiveService<SqliteArchive>,
    session_id: &str,
    out: Option<PathBuf>,
) -> blackbox_application::Result<()> {
    let md = archive.export_markdown(session_id)?;
    match out {
        Some(path) => {
            std::fs::write(&path, &md).map_err(|e| {
                blackbox_application::ArchiveError::Storage(format!(
                    "write {}: {e}",
                    path.display()
                ))
            })?;
            println!("exported to {}", path.display());
        }
        None => print!("{md}"),
    }
    Ok(())
}
