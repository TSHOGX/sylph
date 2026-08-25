# Database Schema

Sylph Web uses SQLite for the live session journal and TraceDB for stable cross-agent history.

## Journal (journal.db)

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
  backend_session_id TEXT,
  started_at_ms INTEGER NOT NULL,
  ended_at_ms INTEGER,
  reset_reason TEXT,
  source_trace_session_id TEXT,
  resulting_trace_session_id TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'ended'))
);

CREATE INDEX incarnations_session_idx
  ON backend_incarnations(session_id, started_at_ms DESC);
```

An incarnation is a row rather than a mutated field, so a reset is recorded
instead of overwritten. `backend_session_id` is nullable because the row is
created before the runner reports the id it chose; `ended_at_ms` and
`reset_reason` are written when it is retired, and the two trace columns record
which archived session it continued from and which one it became. There is no
unique index on `backend_session_id`: an agent may reuse an id across
incarnations, and failing an insert on that is worse than recording it twice.

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
  incarnation_id TEXT REFERENCES backend_incarnations(id),
  status TEXT NOT NULL CHECK (status IN (
    'queued', 'starting', 'running', 'stopping',
    'completed', 'failed', 'cancelled', 'interrupted'
  )),
  request_text TEXT NOT NULL DEFAULT '',
  command_invocation_id TEXT,
  idempotency_key TEXT,
  error_code TEXT,
  error_message TEXT,
  runner_exit_code INTEGER,
  created_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  stopped_at_ms INTEGER,
  UNIQUE(session_id, idempotency_key)
);

CREATE INDEX turns_session_idx ON turns(session_id, created_at_ms DESC);
CREATE INDEX turns_status_idx ON turns(status)
  WHERE status IN ('queued', 'starting', 'running', 'stopping');
```

`idempotency_key` is nullable, so a turn the server originates carries none while
the `UNIQUE(session_id, idempotency_key)` constraint still de-duplicates every
client retry. `command_invocation_id` links a turn to an agent command rather than
to typed text. The partial `turns_status_idx` covers exactly the in-flight states,
which is the query restart reconciliation runs.

Status transitions:
- `queued` → `starting` → `running` → `completed` / `failed` / `cancelled`
- `running` → `stopping` → `cancelled`
- Any in-flight status → `interrupted` (on service restart)

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
  stability TEXT NOT NULL DEFAULT 'stable'
    CHECK (stability IN ('ephemeral', 'stable')),
  source TEXT NOT NULL DEFAULT 'server'
    CHECK (source IN ('runner', 'server', 'tracedb')),
  created_at_ms INTEGER NOT NULL,
  reconciled_at_ms INTEGER,
  UNIQUE(session_id, seq)
);

CREATE INDEX events_session_idx ON events(session_id, seq);
CREATE INDEX events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;
```

Event lifecycle:
- `ephemeral` events (`started`, `delta`) exist only in live projection
- `stable` events (`completed`) are persisted to journal
- `reconciled_at_ms` is where an ingestion watermark would be recorded; nothing
  writes it yet, which is why pruning is still future work
- Once TraceDB ingests a turn, stable events can be pruned after retention window

`UNIQUE(session_id, seq)` is what makes SSE resume-by-sequence safe: a replayed
frame cannot be journalled twice. `seq` is per session, not per turn, so
`events_session_idx` orders by it rather than by timestamp — two events written in
the same millisecond still have one defined order.

#### pending_requests

What an agent is blocked on, waiting for a human. A resource rather than a stream
message, because the agent waits until someone answers: a client closing its laptop
must not strand it, and the answer has to be possible from a frontend that was not
attached when the request was raised.

```sql
CREATE TABLE pending_requests (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT NOT NULL,
  adapter_request_id TEXT NOT NULL,
  kind TEXT NOT NULL,                  -- approval | dialog | elicitation | choice
  tool TEXT,
  tool_use_id TEXT,
  title TEXT NOT NULL DEFAULT '',
  detail_json TEXT NOT NULL DEFAULT '{}',
  options_json TEXT NOT NULL DEFAULT '[]',
  form_json TEXT,                      -- the declared input, when there is one
  default_option TEXT,
  risk TEXT,
  allow_persistent INTEGER NOT NULL DEFAULT 1,
  expires_at_ms INTEGER,
  status TEXT NOT NULL DEFAULT 'pending',  -- pending | resolved | abandoned
  chosen_option TEXT,
  resolved_input_json TEXT,
  resolved_by TEXT,
  created_at_ms INTEGER NOT NULL,
  resolved_at_ms INTEGER
);

CREATE INDEX pending_requests_open
  ON pending_requests(session_id, created_at_ms) WHERE status = 'pending';
CREATE INDEX pending_requests_by_adapter_id
  ON pending_requests(adapter_request_id, status);
```

`pending_requests_open` is partial because resolved rows accumulate without bound and
never appear in the hot "what is this session waiting on" query.

`adapter_request_id` is the adapter's own id, scoped to one link and never serialized
to a client — a client-supplied adapter id would be a way to answer a request that
was never raised. Clients address a request by `id`.

`status = 'pending'` in the resolve UPDATE's WHERE clause is the entire concurrency
control: two clients answering at once both run it, SQLite serializes them, and
exactly one gets a row back. A restart marks every still-pending row `abandoned`
rather than leaving it answerable, because no link survives a restart — but the row
is kept, so the timeline can explain why a turn stopped instead of showing a gap.

`form_json` is the input the request declared it wants collected, and absent is
meaningful rather than missing: a request with no form is an approval, whose subject
is the one proposed tool input in `detail_json`. See `docs/session-protocol.md`
§ Request forms.

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

Numbered SQL migrations in `backend/migrations/`, applied by `sqlx::migrate!` on
startup:

```
001_initial_schema.sql
002_pending_requests.sql       -- what an agent is blocked on
003_agent_event_vocabulary.sql
004_request_affordances.sql    -- allow_persistent, expires_at_ms
005_event_span.sql             -- span, for a delegated trajectory's parent
006_turn_origin.sql            -- origin, for an agent-initiated turn
007_request_form.sql           -- form_json
```

Attachments and commands need no migration of their own: attachments are staged on
disk under the data directory rather than in a table, and a command invocation is a
column on `turns`. `app_meta.schema_version` is reported by `/api/v1/health` so a
packaged install can be checked against the binary running it.

Migration rules:
- Each migration is idempotent (use `IF NOT EXISTS`)
- Migrations are append-only (never edit a deployed migration)
- Before deployment, schemas are disposable (change in place). Treat installed
  state, not a date, as the signal that this has flipped

The append-only rule is not merely tidiness here: `sqlx` checksums each migration
file, so editing a deployed one makes every existing journal refuse to open. That is
why `004` and `005` add columns to the table `002` created rather than amending it.

There are no `sqlx::query!` macros here, only runtime `sqlx::query`, so there is no
`DATABASE_URL` and no `.sqlx` offline directory. A schema mistake surfaces in the
store tests rather than at compile time, which is why those tests matter.
