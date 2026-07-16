# Contributing to Orangebox

Thanks for helping make AI coding sessions crash-proof. This guide covers
setup, the architecture you'll be working in, and the contribution paths
that help most.

## Setup

```sh
git clone https://github.com/samimohameed/orangebox
cd orangebox
cargo test                          # everything should be green
cargo run -p orangebox -- scan  # ingest your own machine's sessions
```

Rust stable is the only requirement. The web UI (`ui/`) is React +
TypeScript + Vite; you only need Node if you're changing the UI:

```sh
cargo run -p orangebox -- ui    # backend on :7171
cd ui && npm install && npm run dev # hot reload, /api proxied
npm run build                       # regenerate crates/cli/assets/index.html
                                    # (committed — the binary embeds it)
```

## Architecture in 30 seconds

Clean Architecture with the dependency rule enforced by crate boundaries:

```
crates/domain          entities — zero dependencies, keep it that way
crates/application     ports (traits) + services; no I/O, no SQLite
crates/infrastructure  SQLite repository, tool adapters, file watcher
crates/cli             composition root: CLI + web server + DTOs
```

Inner crates must never depend on outer ones — Cargo won't let you, so if
you feel the need, the design wants something else (usually a new port).

## The #1 contribution: tool adapters

Support for a new AI tool (Cursor, Copilot, Windsurf, Aider…) is one
self-contained module. The recipe:

1. **Map the storage.** Find where the tool persists chat on disk. Document
   everything you learn in [FORMATS.md](FORMATS.md) — locations, schemas,
   field meanings, and the tool version you observed. Clean-room notes
   only: never copy vendor code or `.proto` files.
2. **Implement `ToolAdapter`** (`crates/application/src/ports.rs`) in
   `crates/infrastructure/src/adapters/<tool>.rs`. Ground rules:
   - **Read-only**: never write to the tool's files; copy databases before
     opening them (see `DbSnapshot` in `antigravity.rs`).
   - **Idempotent**: same file in → same message ids out, so re-ingestion
     never duplicates.
   - **Never fatal**: torn lines, unknown schema versions, and corrupt
     blobs degrade to "skipped with a warning", not errors.
3. **Add fixture tests**: build a synthetic storage file in the test
   (sanitized — no real conversations) and assert sessions, roles, and
   content. See `claude_code.rs` and `antigravity.rs` tests for the
   pattern.
4. Register the adapter in `adapters()` in `crates/cli/src/main.rs` and
   add a row to the README's supported-tools table.

## Pull requests

- `cargo test` and `cargo clippy --workspace -- -D warnings` must pass
  (CI runs both on macOS, Ubuntu, and Windows).
- Keep PRs focused; adapter PRs should touch one adapter.
- If you changed the UI, commit the rebuilt `crates/cli/assets/index.html`.
- Update FORMATS.md when you learn anything new about a tool's storage —
  that file is the project's collective memory.

## Principles that are not up for debate

- **Local-only**: no network calls, no telemetry, ever.
- **Read-only** over other tools' storage.
- **Raw data is sacred**: when parsing fails, prefer archiving something
  over dropping bytes.

## Not sure where to start?

Issues labeled `good first issue` are scoped for a first contribution, and
`help wanted` marks where we most need hands. Open a discussion issue
before large changes so nobody builds the same thing twice.
