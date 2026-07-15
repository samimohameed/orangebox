# Tool storage formats

Everything Blackbox knows about each tool's on-disk format, with the tool
version it was observed on. Update this file whenever an adapter learns
something new — this knowledge is the project's core asset and its biggest
maintenance burden.

## Claude Code (observed: v2.1.205, macOS, July 2026)

**Location:** `~/.claude/projects/<project-slug>/<session-uuid>.jsonl`

- Project slug is the workspace path with `/` replaced by `-`
  (e.g. `-Users-samimohamed-Documents-mac-cleaner`).
- Append-only JSONL, one event per line, written as the conversation
  happens — near-zero loss on power cut.

**Line types** (`type` field): `user`, `assistant`, `queue-operation`,
others (summaries, hooks). Conversation lines carry:

| Field | Notes |
| --- | --- |
| `uuid` | Message id, stable across reads |
| `sessionId` | Session uuid (= filename) |
| `timestamp` | RFC 3339 with millis |
| `cwd` | Workspace path (present on user lines) |
| `message.role` | `user` / `assistant` |
| `message.content` | String (user) or array of blocks (assistant) |

**Assistant content blocks:** `thinking` (+ opaque `signature` — do not
ingest), `text`, `tool_use` (`name`, `input`), `tool_result`.

**Crash behavior:** the final line may be torn (no trailing newline /
truncated JSON). Parsers must skip unparseable lines, never fail the file.

## Antigravity (observed: May 2026 build, macOS)

**Location:** `~/Library/Application Support/Antigravity IDE/`

- VS Code-lineage layout: `User/globalStorage/state.vscdb` (SQLite),
  per-workspace `User/workspaceStorage/<hash>/state.vscdb`.
- Agent trajectory data found locally so far: key
  `antigravityUnifiedStateSync.trajectorySummaries` in the global
  `state.vscdb` `ItemTable` — base64-encoded protobuf, ~17 KB, summaries
  only (ids, titles, terminal snippets, file paths). Full trajectory
  content was NOT found on disk; presumed in-memory + server-side.
- Also relevant: `Backups/` (hot-exit unsaved buffers), `User/History/`
  (local file edit history — captures agent-written plans in files).

**Open questions (Phase 0 experiment):** what survives `kill -9`
mid-session, online vs offline; protobuf field map of a trajectory
summary (`protoc --decode_raw`).

## Cursor (not yet observed on this machine)

Reported: chat in `~/Library/Application Support/Cursor/User/`
`globalStorage/state.vscdb`, keys like `workbench.panel.aichat.*` /
`composer.composerData`. Verify against a real install before building
the adapter.

## VS Code Copilot Chat (not yet observed on this machine)

Reported: JSON session files under
`~/Library/Application Support/Code/User/workspaceStorage/<hash>/chatSessions/`.
Verify before building the adapter.
