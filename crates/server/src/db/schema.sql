-- Initial control authority schema. No event, turn, block or projection data.

CREATE TABLE IF NOT EXISTS app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS subjects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  is_admin INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS env_grants (
  subject_id TEXT NOT NULL,
  environment_id TEXT NOT NULL,
  capability TEXT NOT NULL CHECK (capability IN ('observe', 'control')),
  issuer_subject_id TEXT,
  issued_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER,
  consumed_at_ms INTEGER,
  PRIMARY KEY (subject_id, environment_id, capability),
  FOREIGN KEY (subject_id) REFERENCES subjects(id) ON DELETE CASCADE,
  FOREIGN KEY (issuer_subject_id) REFERENCES subjects(id) ON DELETE SET NULL,
  FOREIGN KEY (environment_id) REFERENCES env_installations(environment_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sessions (
  environment  TEXT NOT NULL,
  agent        TEXT NOT NULL,
  native_id    TEXT NOT NULL,

  workdir          TEXT,
  title            TEXT,
  inject_identity  INTEGER NOT NULL,
  fidelity         TEXT NOT NULL CHECK (fidelity IN ('stream','blocks','messages')),
  flags_json       TEXT NOT NULL DEFAULT '{}',
  status           TEXT NOT NULL DEFAULT 'idle' CHECK (status IN ('idle','running')),
  created_at_ms    INTEGER NOT NULL,
  updated_at_ms    INTEGER NOT NULL,

  PRIMARY KEY (environment, agent, native_id)
);

CREATE TABLE IF NOT EXISTS env_installations (
  environment_id TEXT PRIMARY KEY,
  installation_id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  origin TEXT NOT NULL,
  grant_authority TEXT NOT NULL DEFAULT 'machine'
    CHECK (grant_authority IN ('machine', 'server')),
  token_hash TEXT NOT NULL,
  connected INTEGER NOT NULL DEFAULT 0,
  connection_epoch INTEGER NOT NULL DEFAULT 0,
  last_seen_at_ms INTEGER,
  reachable_via TEXT,
  agents_json TEXT NOT NULL DEFAULT '[]',
  facts_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS env_pairings (
  code_hash TEXT PRIMARY KEY,
  expires_at_ms INTEGER NOT NULL,
  used_at_ms INTEGER
);

CREATE TABLE IF NOT EXISTS env_invites (
  code_hash TEXT PRIMARY KEY,
  environment_id TEXT NOT NULL,
  installation_id TEXT NOT NULL,
  capability TEXT NOT NULL CHECK (capability IN ('observe', 'control')),
  expires_at_ms INTEGER NOT NULL,
  consumed_at_ms INTEGER,
  FOREIGN KEY (environment_id) REFERENCES env_installations(environment_id) ON DELETE CASCADE,
  FOREIGN KEY (installation_id) REFERENCES env_installations(installation_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS env_command_results (
  environment_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  idempotency TEXT NOT NULL CHECK (idempotency IN ('replayable', 'at_most_once')),
  outcome_json TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (environment_id, request_id)
);

CREATE TABLE IF NOT EXISTS import_jobs (
  id TEXT PRIMARY KEY,
  environment_id TEXT NOT NULL,
  origin_subject TEXT NOT NULL,
  workdir TEXT,
  agents_json TEXT,
  mode TEXT NOT NULL CHECK (mode IN ('import_once', 'import_and_watch')),
  idempotency_key TEXT NOT NULL,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('queued','running','completed','cancelled','failed')),
  scan_complete INTEGER NOT NULL DEFAULT 0,
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(environment_id, origin_subject, idempotency_key)
);

CREATE TABLE IF NOT EXISTS import_items (
  job_id TEXT NOT NULL REFERENCES import_jobs(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  order_tie INTEGER NOT NULL DEFAULT 0,
  agent TEXT NOT NULL,
  native_id TEXT NOT NULL,
  title TEXT,
  workdir TEXT,
  status TEXT NOT NULL CHECK (status IN ('pending','running','imported','refreshed','skipped','failed')),
  reason TEXT,
  error_code TEXT,
  source_revision TEXT,
  PRIMARY KEY(job_id, agent, native_id),
  UNIQUE(job_id, ordinal)
);

CREATE INDEX IF NOT EXISTS env_grants_environment_idx ON env_grants(environment_id, capability);

CREATE INDEX IF NOT EXISTS sessions_updated_idx ON sessions(updated_at_ms DESC);

CREATE INDEX IF NOT EXISTS sessions_workdir_idx ON sessions(environment, workdir);

CREATE INDEX IF NOT EXISTS env_pairings_live_idx
  ON env_pairings(expires_at_ms)
  WHERE used_at_ms IS NULL;

CREATE INDEX IF NOT EXISTS env_invites_live_idx
  ON env_invites(environment_id, expires_at_ms)
  WHERE consumed_at_ms IS NULL;

CREATE INDEX IF NOT EXISTS import_jobs_environment_idx ON import_jobs(environment_id, created_at_ms DESC);

CREATE INDEX IF NOT EXISTS import_items_native_idx ON import_items(agent, native_id, job_id);

CREATE TABLE IF NOT EXISTS current_requests(
 environment_id TEXT NOT NULL,
 session_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 request_json TEXT NOT NULL,
 connection_epoch INTEGER NOT NULL,
 state TEXT NOT NULL DEFAULT 'waiting' CHECK(state IN ('waiting','resolving','resolved')),
 resolution_command TEXT,
 PRIMARY KEY(session_id,request_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS current_requests_environment ON current_requests(environment_id,state);
CREATE INDEX IF NOT EXISTS current_requests_resolution ON current_requests(resolution_command);
CREATE INDEX IF NOT EXISTS current_waiting_requests ON current_requests(session_id,request_id) WHERE state='waiting';
CREATE TABLE IF NOT EXISTS session_control_state(
 environment_id TEXT NOT NULL,
 session_id TEXT PRIMARY KEY,
 lease TEXT,
 native_turn_id TEXT,
 active INTEGER NOT NULL CHECK(active IN(0,1)),
 connection_epoch INTEGER NOT NULL
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS session_control_environment ON session_control_state(environment_id);
