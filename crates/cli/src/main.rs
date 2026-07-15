//! Blackbox CLI — composition root.
//!
//! This binary is the only place where concrete infrastructure (SQLite,
//! adapters, watcher) is wired into the application use cases.

use std::path::PathBuf;
use std::time::Duration;

use blackbox_application::ports::ToolAdapter;
use blackbox_application::usecases;
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
}

fn adapters() -> Vec<Box<dyn ToolAdapter>> {
    let mut list: Vec<Box<dyn ToolAdapter>> = Vec::new();
    if let Some(claude) = ClaudeCodeAdapter::new_default() {
        list.push(Box::new(claude));
    }
    // Phase 2: Antigravity. Phase 4: Cursor, Copilot.
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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "blackbox=info,warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(blackbox_infrastructure::default_db_path);

    let mut archive = match SqliteArchive::open(&db_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: cannot open archive at {}: {e}", db_path.display());
            std::process::exit(1);
        }
    };

    let result = match cli.command {
        Command::Scan => cmd_scan(&mut archive),
        Command::Watch => cmd_watch(&mut archive),
        Command::Search { query, limit } => cmd_search(&archive, &query.join(" "), limit),
        Command::Timeline { limit } => cmd_timeline(&archive, limit),
        Command::Status => cmd_status(&archive, &db_path),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn cmd_scan(archive: &mut SqliteArchive) -> blackbox_application::Result<()> {
    let adapters = adapters();
    let reports = usecases::scan_all(&adapters, archive)?;
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

fn cmd_watch(archive: &mut SqliteArchive) -> blackbox_application::Result<()> {
    let adapters = adapters();

    // Catch up on anything written while we weren't running, then tail.
    let reports = usecases::scan_all(&adapters, archive)?;
    let backfilled: usize = reports.iter().map(|r| r.new_messages).sum();
    println!("backfill: {backfilled} new message(s); watching for changes (ctrl-c to stop)");

    let roots: Vec<PathBuf> = adapters.iter().flat_map(|a| a.watch_roots()).collect();
    let watcher = FsWatcher::watch(&roots, Duration::from_secs(2))?;

    while let Some(paths) = watcher.next_changed_paths() {
        for path in paths {
            match usecases::ingest_changed_path(&adapters, &path, archive) {
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
    archive: &SqliteArchive,
    query: &str,
    limit: usize,
) -> blackbox_application::Result<()> {
    let hits = usecases::search(archive, query, limit)?;
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

fn cmd_timeline(archive: &SqliteArchive, limit: usize) -> blackbox_application::Result<()> {
    let sessions = usecases::timeline(archive, limit)?;
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

fn cmd_status(archive: &SqliteArchive, db_path: &PathBuf) -> blackbox_application::Result<()> {
    let stats = usecases::stats(archive)?;
    println!("archive: {}", db_path.display());
    println!("sessions: {}", stats.sessions);
    println!("messages: {}", stats.messages);
    for (tool, count) in &stats.tools {
        println!("  {tool}: {count} session(s)");
    }
    Ok(())
}

fn short_project(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
