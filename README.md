# Blackbox 📼

**A flight recorder for AI coding sessions.** Blackbox watches the local
storage of AI coding tools (Claude Code today; Antigravity, Cursor and
Copilot next) and journals every conversation into a crash-safe, searchable
archive — so a power cut, a force-quit, or a tool losing its own history
never costs you a conversation or an implementation plan again.

**Local-only, forever.** Blackbox makes no network calls, sends no
telemetry, and never writes to any tool's storage — it only reads.

## Usage

```sh
blackbox scan            # backfill: archive every existing session
blackbox watch           # record continuously (foreground for now)
blackbox ui              # local web UI — browse, search, recover
                         # (keeps recording while open)
blackbox search "plan"   # full-text search across all tools
blackbox timeline        # recent sessions, newest first
blackbox show <id>       # print one session's transcript
blackbox export <id>     # Markdown recovery document (-o file.md)
blackbox status          # archive stats
```

## Recovering a lost session

Sessions can't be injected back into the tools' proprietary histories,
so recovery works forward, not backward:

- **Claude Code** — sessions usually survive locally; run
  `claude --resume <session-uuid>` in the project directory, or use the
  exported Markdown.
- **Antigravity / others** — open the session in `blackbox ui`, press
  **Copy recovery prompt**, paste it into a fresh session, and ask the
  agent to continue from it. The export carries everything Blackbox
  recorded (for Antigravity: titles, progress, terminal snippets —
  full trajectories never touch local disk).

The archive lives in your platform data directory
(macOS: `~/Library/Application Support/blackbox/archive.db`);
override with `--db <path>`.

## Architecture

Clean Architecture, with the dependency rule enforced by crate boundaries —
inner layers cannot reference outer ones because Cargo won't let them:

```
crates/
  domain/          entities (Session, Message, ToolKind) — zero dependencies
  application/     use cases + ports (ToolAdapter, ArchiveRepository)
  infrastructure/  SQLite (WAL + FTS5), tool adapters, file watcher
  cli/             clap binary — the composition root
```

Ground rules:

- Adapters are **read-only** over tool storage and **idempotent**
  (re-ingesting a file never duplicates messages).
- Raw parsing failures are never fatal: a torn line from a crash is
  skipped, the rest of the file ingests.
- Format knowledge lives in [FORMATS.md](FORMATS.md).

## Development

```sh
cargo test         # unit tests incl. golden-fixture parser tests
cargo run -p blackbox-cli -- scan
```
