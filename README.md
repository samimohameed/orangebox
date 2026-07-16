# Blackbox 📼

![CI](https://github.com/samimohameed/blackbox/actions/workflows/ci.yml/badge.svg)

**A flight recorder for AI coding sessions.** Blackbox watches the local
storage of AI coding tools (Claude Code today; Antigravity, Cursor and
Copilot next) and journals every conversation into a crash-safe, searchable
archive — so a power cut, a force-quit, or a tool losing its own history
never costs you a conversation or an implementation plan again.

**Local-only, forever.** Blackbox makes no network calls, sends no
telemetry, and never writes to any tool's storage — it only reads.

![Blackbox demo: search every conversation, open a recovered Antigravity trajectory, copy the recovery prompt](docs/demo.gif)

## Supported tools

| Tool | What gets recorded | Status |
| --- | --- | --- |
| Claude Code | Full conversations (JSONL journal) | ✅ |
| Antigravity IDE | Full trajectories: prompts, model reasoning, tool activity, file edits, terminal output | ✅ (trajectories before ~May 2026 use an older format, summaries only) |
| Cursor | — | planned |
| VS Code Copilot Chat | — | planned |

**Platform:** macOS today. The core is portable Rust; Windows/Linux need
only the per-tool storage paths — contributions welcome.

## Getting started

Requires the [Rust toolchain](https://rustup.rs) (1.80+). No Node or other
runtime needed — the web UI ships prebuilt inside the binary.

```sh
git clone https://github.com/samimohameed/blackbox
cd blackbox
cargo install --path crates/cli
```

Then:

```sh
blackbox scan   # backfill everything your tools already have on disk
blackbox ui     # open http://127.0.0.1:7171 — browse, search, recover
```

`scan` is idempotent — run it as often as you like. `ui` keeps recording
in the background while it's open; for terminal-only recording use
`blackbox watch`.

### Crash-safety, verified

The archive uses SQLite in WAL mode. `kill -9` mid-ingestion leaves a
consistent database (`PRAGMA integrity_check` passes) and the next scan
picks up exactly what was missed — no duplicates, no corruption. Recorded
conversations are stored as-is (they are already plaintext in each tool's
own storage); secret redaction is on the roadmap.

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
  agent to continue from it. Antigravity exports carry the full recorded
  trajectory: your prompts, the model's reasoning, tool activity, file
  contents, and terminal output (recovered from
  `~/.gemini/antigravity-ide/conversations/`).

The archive lives in your platform data directory
(macOS: `~/Library/Application Support/blackbox/archive.db`);
override with `--db <path>`.

## Architecture

Clean Architecture, with the dependency rule enforced by crate boundaries —
inner layers cannot reference outer ones because Cargo won't let them:

```
crates/
  domain/          entities (Session, Message, ToolKind) — zero dependencies
  application/     ports (ToolAdapter, ArchiveRepository interfaces) +
                   services (RecorderService, ArchiveService) with
                   constructor-injected dependencies
  infrastructure/  SQLite repository (WAL + FTS5), tool adapters, watcher
  cli/             clap binary + web server — the composition root, where
                   concrete types are constructed and injected; HTTP DTOs
                   live in src/dto/
ui/                React + TypeScript + Vite app, embedded into the binary
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

The GUI is a React + TypeScript + Vite app in `ui/`, built into a single
self-contained `ui/dist/index.html` that the Rust binary embeds at compile
time (so `cargo install` needs no JS toolchain — the built file is
committed). Working on the GUI:

```sh
blackbox ui                         # backend on :7171
cd ui && npm install && npm run dev # hot-reload dev server, /api proxied
npm run build                       # rebuild the embedded bundle,
                                    # then cargo build
```
