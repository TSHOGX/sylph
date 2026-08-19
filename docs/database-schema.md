# Database Schema

Sylph Web uses SQLite for the live session journal and TraceDB for stable cross-agent history.

## Live Journal (live.db)

The live journal stores active sessions, turns, and recent events. It uses WAL mode for concurrent access and numbered migrations for schema evolution.

### Tables

#### app_meta

Application metadata and migration tracking.

```sql
CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

Tracked keys:
- `schema_version` — current migration number
- `instance_id` — process-local identifier

#### sessions

Session records with origin and policy modes.

```sql
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  origin TEXT NOT NULL CHECK (origin IN ('sylph', 'native')),
  identity_mode TEXT NOT NULL CHECK (identity_mode IN ('sylph', 'preserve')),
  lifecycle_mode TEXT NOT NULL CHECK (lifecycle_mode IN ('sylph', 'preserve')),
  agent TEXT NOT NULL,
  backend_session_id TEXT,
  trace_session_id TEXT,
  workdir TEXT NOT NULL,
  title TEXT,
  flags_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'idle' CHECK (status IN ('idle', 'running')),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE INDEX sessions_updated_idx ON sessions(updated_at_ms DESC);
CREATE INDEX sessions_trace_idx ON sessions(trace_session_id) 
  WHERE trace_session_id IS NOT NULL;
```

Fields:
- `id` — UUID v4, generated server-side
- `origin` — `sylph` (created by WebApp) or `native` (imported from TUI/CLI)
- `identity_mode` — `sylph` (inject SOUL.md) or `preserve` (original agent context)
- `lifecycle_mode` — `sylph` (24h timeout, 04:00 reset) or `preserve` (no implicit lifecycle)
- `agent` — runner name from discovery (e.g., `claude`, `codex`)
- `backend_session_id` — opaque agent-native session ID
- `trace_session_id` — TraceDB session ID after ingestion
- `workdir` — absolute path to working directory
- `title` — optional user-provided title
- `flags_json` — JSON object of agent-specific flags (model, effort, etc.)
- `status` — `idle` or `running`

#### backend_incarnations

Tracks backend session lifecycle resets for Sylph-managed sessions.

```sql
CREATE TABLE backend_incarnations (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  backend_session_id TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  last_used_at_ms INTEGER NOT NULL
);

CREATE INDEX incarnations_session_idx ON backend_incarnations(session_id);
CREATE UNIQUE INDEX incarnations_backend_idx 
  ON backend_incarnations(backend_session_id);
```

An incarnation is created on first turn and discarded after:
- 24 hours idle (for Sylph sessions)
- Local time crosses 04:00 (for Sylph sessions)
- Never for native sessions (single incarnation, never expires)

#### turns

Turn execution records.

```sql
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  incarnation_id TEXT REFERENCES backend_incarnations(id) ON DELETE SET NULL,
  idempotency_key TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'queued' 
    CHECK (status IN ('queued', 'starting', 'running', 'stopping', 
                      'completed', 'failed', 'cancelled', 'interrupted')),
  request_text TEXT NOT NULL,
  error_code TEXT,
  error_message TEXT,
  runner_exit_code INTEGER,
  created_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  stopped_at_ms INTEGER
);

CREATE INDEX turns_session_idx ON turns(session_id, created_at_ms DESC);
CREATE UNIQUE INDEX turns_idempotency_idx 
  ON turns(session_id, idempotency_key);
```

Status transitions:
- `queued` → `starting` → `running` → `completed` / `failed` / `cancelled`
- `running` → `stopping` → `stopped`
- Any status → `interrupted` (on service restart)

#### events

Event stream journal (bounded live storage).

```sql
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL 
    CHECK (kind IN ('status', 'system', 'user', 'thinking', 'assistant',
                    'tool_call', 'tool_result', 'usage', 'error', 'approval')),
  phase TEXT CHECK (phase IN ('started', 'delta', 'completed')),
  stream_id TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  stability TEXT NOT NULL DEFAULT 'ephemeral'
    CHECK (stability IN ('ephemeral', 'stable')),
  source TEXT NOT NULL DEFAULT 'runner'
    CHECK (source IN ('runner', 'server', 'tracedb')),
  created_at_ms INTEGER NOT NULL
);

CREATE INDEX events_turn_idx ON events(turn_id, seq);
CREATE INDEX events_session_idx ON events(session_id, created_at_ms DESC);
```

Event lifecycle:
- `ephemeral` events (`started`, `delta`) exist only in live projection
- `stable` events (`completed`) are persisted to journal
- Once TraceDB ingests a turn, stable events can be pruned after retention window

## TraceDB

TraceDB is accessed only through its JSON protocol. The service never queries TraceDB's SQLite schema directly.

Queries:
- `list` — list sessions with filters
- `show` — fetch session trajectory
- `search` — full-text search across sessions

Sylph Web reads from TraceDB for:
- Session list in UI (mixed with live sessions)
- Native session import (create a lightweight continuation record)
- Cross-agent search

TraceDB writes come from the agent's native store (ingestion is external to Sylph Web).

## Migrations

Numbered SQL migrations in `backend/migrations/`:

```
001_initial_schema.sql
002_add_attachments.sql
003_add_commands.sql
...
```

Migrations are applied transactionally on startup. The `app_meta.schema_version` tracks the current version.

Migration rules:
- Each migration is idempotent (use `IF NOT EXISTS`)
- Migrations are append-only (never edit a deployed migration)
- Before deployment, schemas are disposable (change in place)
