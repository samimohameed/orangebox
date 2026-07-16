//! Orangebox CLI — composition root.
//!
//! This binary is the only place where concrete infrastructure (SQLite,
//! adapters, watcher) is constructed and injected into the application
//! services.

mod dto;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use orangebox_application::ports::ToolAdapter;
use orangebox_application::services::{ArchiveService, RecorderService};
use orangebox_infrastructure::adapters::antigravity::AntigravityAdapter;
use orangebox_infrastructure::adapters::claude_code::ClaudeCodeAdapter;
use orangebox_infrastructure::sqlite::SqliteArchive;
use orangebox_infrastructure::watcher::FsWatcher;
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "orangebox", version, about = "Flight recorder for AI coding sessions")]
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
    /// Install the always-on recorder (launchd agent, starts at login).
    Install,
    /// Stop and remove the always-on recorder.
    Uninstall,
    /// Health check: daemon state, archive, watch paths.
    Doctor,
    /// Delete sessions idle for more than N days (explicit, never automatic).
    Prune {
        #[arg(long)]
        keep_days: u32,
        /// Show what would be deleted without deleting it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the recorder loop (what the launchd agent executes).
    #[command(hide = true)]
    Daemon,
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
                .unwrap_or_else(|_| "orangebox=info,warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(orangebox_infrastructure::default_db_path);

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
        Command::Install => cmd_install(),
        Command::Uninstall => cmd_uninstall(),
        Command::Doctor => cmd_doctor(&ArchiveService::new(repo), &db_path),
        Command::Prune { keep_days, dry_run } => {
            cmd_prune(ArchiveService::new(repo), keep_days, dry_run)
        }
        Command::Daemon => cmd_daemon(RecorderService::new(repo, adapters())),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn cmd_scan(mut recorder: RecorderService<SqliteArchive>) -> orangebox_application::Result<()> {
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

fn cmd_watch(mut recorder: RecorderService<SqliteArchive>) -> orangebox_application::Result<()> {
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
) -> orangebox_application::Result<()> {
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
) -> orangebox_application::Result<()> {
    let sessions = archive.timeline(limit)?;
    if sessions.is_empty() {
        println!("archive is empty — run `orangebox scan` first");
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
    db_path: &std::path::Path,
) -> orangebox_application::Result<()> {
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
) -> orangebox_application::Result<()> {
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
) -> orangebox_application::Result<()> {
    let md = archive.export_markdown(session_id)?;
    match out {
        Some(path) => {
            std::fs::write(&path, &md).map_err(|e| {
                orangebox_application::ArchiveError::Storage(format!(
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

fn cmd_daemon(mut recorder: RecorderService<SqliteArchive>) -> orangebox_application::Result<()> {
    tracing::info!("orangebox daemon starting");
    let reports = recorder.scan_all()?;
    let backfilled: usize = reports.iter().map(|r| r.new_messages).sum();
    tracing::info!(backfilled, "backfill complete; watching");

    let watcher = FsWatcher::watch(&recorder.watch_roots(), Duration::from_secs(2))?;
    while let Some(paths) = watcher.next_changed_paths() {
        for path in paths {
            match recorder.ingest_changed_path(&path) {
                Ok(Some(report)) if report.new_messages > 0 => {
                    tracing::info!(tool = report.tool, new = report.new_messages, "recorded");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(path = %path.display(), "ingest failed: {e}"),
            }
        }
    }
    Ok(())
}

fn cmd_install() -> orangebox_application::Result<()> {
    let binary = std::env::current_exe()
        .map_err(|e| orangebox_application::ArchiveError::Storage(format!("current_exe: {e}")))?;
    let plist = orangebox_infrastructure::launchd::install(&binary.display().to_string())?;
    println!("installed launchd agent: {}", plist.display());
    println!("logs: {}", orangebox_infrastructure::launchd::log_path().display());
    println!("The recorder now starts at login and restarts if it dies.");
    println!("Check it any time with `orangebox doctor`.");
    Ok(())
}

fn cmd_uninstall() -> orangebox_application::Result<()> {
    orangebox_infrastructure::launchd::uninstall()?;
    println!("always-on recorder stopped and removed");
    Ok(())
}

fn cmd_doctor(
    archive: &ArchiveService<SqliteArchive>,
    db_path: &std::path::Path,
) -> orangebox_application::Result<()> {
    let ok = |b: bool| if b { "ok  " } else { "WARN" };

    let installed = orangebox_infrastructure::launchd::is_installed();
    let running = orangebox_infrastructure::launchd::is_running();
    println!("[{}] daemon installed: {installed}", ok(installed));
    println!("[{}] daemon running:   {running}", ok(running || !installed));
    if !installed {
        println!("       hint: `orangebox install` enables always-on recording");
    }

    let db_size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    println!(
        "[{}] archive: {} ({:.1} MB)",
        ok(db_size > 0),
        db_path.display(),
        db_size as f64 / 1e6
    );

    let stats = archive.stats()?;
    println!("[ok  ] {} sessions, {} messages", stats.sessions, stats.messages);

    let last = archive.timeline(1)?;
    match last.first() {
        Some(s) => {
            let age_h = (Local::now().timestamp_millis() - s.session.last_activity_ms) as f64
                / 3_600_000.0;
            println!(
                "[ok  ] last recorded activity: {} ({:.1}h ago)",
                fmt_ts(s.session.last_activity_ms),
                age_h
            );
        }
        None => println!("[WARN] archive is empty — run `orangebox scan`"),
    }

    for adapter in adapters() {
        for root in adapter.watch_roots() {
            let exists = root.exists();
            println!(
                "[{}] {} watch root: {}",
                ok(exists),
                adapter.tool().slug(),
                root.display()
            );
        }
    }
    Ok(())
}

fn cmd_prune(
    mut archive: ArchiveService<SqliteArchive>,
    keep_days: u32,
    dry_run: bool,
) -> orangebox_application::Result<()> {
    let now_ms = Local::now().timestamp_millis();
    if dry_run {
        let cutoff = now_ms - i64::from(keep_days) * 24 * 60 * 60 * 1000;
        let old: Vec<_> = archive
            .timeline(usize::MAX)?
            .into_iter()
            .filter(|s| s.session.last_activity_ms < cutoff)
            .collect();
        let messages: i64 = old.iter().map(|s| s.message_count).sum();
        println!(
            "dry run: would delete {} session(s) with {} message(s) older than {} day(s)",
            old.len(),
            messages,
            keep_days
        );
        return Ok(());
    }
    let (sessions, messages) = archive.prune(keep_days, now_ms)?;
    println!("pruned {sessions} session(s), {messages} message(s) idle for more than {keep_days} day(s)");
    Ok(())
}
