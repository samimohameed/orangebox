# Tool storage formats

Everything Orangebox knows about each tool's on-disk format, with the tool
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

**Locations:** `~/Library/Application Support/Antigravity IDE/` (VS Code
lineage state) and `~/.gemini/antigravity-ide/` (agent data: conversations,
brain, knowledge — ~148 MB observed).

### Full conversations — `~/.gemini/antigravity-ide/conversations/`

One file per trajectory, named `<trajectory-uuid>.db` (SQLite; trajectories
from ~May 23 2026 and earlier use a `.pb` protobuf format we don't parse
yet). Tables: `steps`, `trajectory_meta`, `trajectory_metadata_blob`,
`executor_metadata`, `gen_metadata`, `battle_mode_infos`,
`parent_references`.

`steps` schema: `idx` (PK), `step_type`, `status`, `metadata` blob,
`step_payload` blob (protobuf), … The payload's field 1 repeats the step
type; field 5 wraps a `{1: {1: secs, 2: nanos}}` timestamp. Text lives at
varying depths — the adapter harvests prose-looking strings recursively.

Observed `step_type` → meaning (mapped 2026-07-16):

| Type | Meaning | Role |
| --- | --- | --- |
| 14 | User message (incl. attached context) | user |
| 15 | Model planning/prose | assistant |
| 5 | Model proposals (markdown docs, edit instructions) | assistant |
| 23 | Task description docs | assistant |
| 85 | Trajectory self-summary | assistant |
| 21 | Terminal command | tool |
| 7 | Command/tool output | tool |
| 8 | File contents viewed/written | tool |
| 9 | Tool action JSON (`toolAction`, `toolSummary`) | tool |
| 132 / 101 | Brain task events / inter-task messages | tool |
| 90 | Ephemeral system reminder | skipped |
| 98 / 99 | Injected conversation history / empty | skipped |

### Summaries — Application Support

- VS Code-lineage layout: `User/globalStorage/state.vscdb` (SQLite),
  per-workspace `User/workspaceStorage/<hash>/state.vscdb`.
- Agent trajectory data found locally: key
  `antigravityUnifiedStateSync.trajectorySummaries` in the global
  `state.vscdb` `ItemTable` — base64 text of protobuf, summaries only.
  Full trajectory content was NOT found on disk; presumed in-memory +
  server-side.
- Also relevant (not yet ingested): `Backups/` (hot-exit unsaved
  buffers), `User/History/` (local file edit history — captures
  agent-written plans in files).

**Decoded structure** (mapped 2026-07-16 by wire-format walking; no
vendor `.proto` files used):

```
outer blob:  repeated field 1 (entry) {
  1: trajectory uuid (str)
  2: { 1: base64(inner summary) }        # double-encoded!
}
inner summary: {
  1:  title (str) — or raw terminal-output text for terminal entries
  2:  step count (varint)
  3:  timestamp {1: unix secs, 2: nanos} — last activity
  4:  conversation uuid (str)
  7:  timestamp — created at
  9:  { 1: workspace URI "file:///..." }
  10: timestamp — another activity marker
  16: secondary counter; 17: workspace metadata msg; 22: enum
}
```

Adapter behavior: ingest via copy-then-read of the `.vscdb` (+ WAL/SHM
sidecars), one session per trajectory, one summary message per session.
Unknown/corrupt blobs degrade to an empty batch with a warning, never an
error.

**Open questions (Phase 0 experiment):** what survives `kill -9`
mid-session, online vs offline.

## Cursor (not yet observed on this machine)

Reported: chat in `~/Library/Application Support/Cursor/User/`
`globalStorage/state.vscdb`, keys like `workbench.panel.aichat.*` /
`composer.composerData`. Verify against a real install before building
the adapter.

## VS Code Copilot Chat (not yet observed on this machine)

Reported: JSON session files under
`~/Library/Application Support/Code/User/workspaceStorage/<hash>/chatSessions/`.
Verify before building the adapter.
