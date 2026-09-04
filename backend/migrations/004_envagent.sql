-- Durable state for paired envagents and inbound delivery.
--
-- The environment registry is the server's authority.  The envagent's spool
-- only remembers what has not been acknowledged; these tables retain pairing,
-- connection leases, command outcomes, and producer receipts across a server
-- restart.

CREATE TABLE IF NOT EXISTS env_installations (
  environment_id TEXT PRIMARY KEY,
  installation_id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  origin TEXT NOT NULL,
  token_hash TEXT NOT NULL,
  connected INTEGER NOT NULL DEFAULT 0,
  connection_epoch INTEGER NOT NULL DEFAULT 0,
  last_seen_at_ms INTEGER,
  reachable_via TEXT,
  agents_json TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS env_pairings (
  code_hash TEXT PRIMARY KEY,
  expires_at_ms INTEGER NOT NULL,
  used_at_ms INTEGER
);

CREATE INDEX IF NOT EXISTS env_pairings_live_idx
  ON env_pairings(expires_at_ms)
  WHERE used_at_ms IS NULL;

CREATE TABLE IF NOT EXISTS env_command_results (
  environment_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  idempotency TEXT NOT NULL CHECK (idempotency IN ('replayable', 'at_most_once')),
  outcome_json TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (environment_id, request_id)
);

CREATE TABLE IF NOT EXISTS env_event_receipts (
  environment_id TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_seq INTEGER NOT NULL CHECK (producer_seq > 0),
  session_env TEXT NOT NULL,
  session_agent TEXT NOT NULL,
  session_native TEXT NOT NULL,
  server_seq INTEGER NOT NULL,
  payload_hash TEXT NOT NULL,
  accepted_at_ms INTEGER NOT NULL,
  PRIMARY KEY (environment_id, producer_id, producer_seq)
);

CREATE INDEX IF NOT EXISTS env_event_receipts_session_idx
  ON env_event_receipts(session_env, session_agent, session_native, server_seq);

-- Server-owned graph edges.  Events remain stored once under their owning
-- session; this table is a read model for delegation and fork provenance.
CREATE TABLE IF NOT EXISTS session_edges (
  parent_env TEXT NOT NULL,
  parent_agent TEXT NOT NULL,
  parent_native TEXT NOT NULL,
  child_env TEXT NOT NULL,
  child_agent TEXT NOT NULL,
  child_native TEXT NOT NULL,
  relation TEXT NOT NULL CHECK (relation IN ('delegation', 'fork')),
  source TEXT NOT NULL DEFAULT 'server',
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (
    parent_env, parent_agent, parent_native,
    child_env, child_agent, child_native,
    relation
  ),
  CHECK (
    parent_env <> child_env OR
    parent_agent <> child_agent OR
    parent_native <> child_native
  )
);

CREATE INDEX IF NOT EXISTS session_edges_child_idx
  ON session_edges(child_env, child_agent, child_native);
