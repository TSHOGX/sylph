-- Initial schema
CREATE TABLE IF NOT EXISTS app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
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

CREATE INDEX IF NOT EXISTS sessions_updated_idx ON sessions(updated_at_ms DESC);
CREATE INDEX IF NOT EXISTS sessions_trace_idx ON sessions(trace_session_id)
  WHERE trace_session_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS backend_incarnations (
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

CREATE INDEX IF NOT EXISTS incarnations_session_idx
  ON backend_incarnations(session_id, started_at_ms DESC);

CREATE TABLE IF NOT EXISTS turns (
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

CREATE INDEX IF NOT EXISTS turns_session_idx ON turns(session_id, created_at_ms DESC);
CREATE INDEX IF NOT EXISTS turns_status_idx ON turns(status)
  WHERE status IN ('queued', 'starting', 'running', 'stopping');

CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN (
    'status', 'system', 'user', 'thinking', 'assistant',
    'tool_call', 'tool_result', 'usage', 'error', 'approval'
  )),
  phase TEXT,
  stream_id TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  stability TEXT NOT NULL DEFAULT 'stable' CHECK (stability IN ('ephemeral', 'stable')),
  source TEXT NOT NULL DEFAULT 'server' CHECK (source IN ('runner', 'server', 'tracedb')),
  created_at_ms INTEGER NOT NULL,
  reconciled_at_ms INTEGER,
  UNIQUE(session_id, seq)
);

CREATE INDEX IF NOT EXISTS events_session_idx ON events(session_id, seq);
CREATE INDEX IF NOT EXISTS events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;

INSERT OR IGNORE INTO app_meta (key, value) VALUES ('schema_version', '1');

