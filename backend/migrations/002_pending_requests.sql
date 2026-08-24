-- Requests an agent raised and a human has not answered.
--
-- Durable rather than in-memory because a pending request blocks the agent, and
-- an approval that lives only on a stream is stranded the moment a client
-- disconnects — closing a laptop lid would leave the agent waiting forever.
-- `docs/control-plane.md` argues this out: the stream is the notification, the
-- resource is the truth.
CREATE TABLE pending_requests (
  id            TEXT PRIMARY KEY,
  session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id       TEXT NOT NULL,
  -- The id the adapter used. A resolve has to echo this, and it is scoped to the
  -- link rather than globally unique, so it is stored beside our own id rather
  -- than used as the primary key.
  adapter_request_id TEXT NOT NULL,
  kind          TEXT NOT NULL,
  tool          TEXT,
  tool_use_id   TEXT,
  title         TEXT NOT NULL DEFAULT '',
  detail_json   TEXT NOT NULL DEFAULT '{}',
  options_json  TEXT NOT NULL DEFAULT '[]',
  default_option TEXT,
  risk          TEXT,
  -- 'pending' | 'resolved' | 'abandoned'
  --
  -- Resolved rows are kept rather than deleted: what was approved, by whom, and
  -- with what edit is exactly the audit trail a permission decision needs, and
  -- deleting the row would leave the timeline showing a tool call with no
  -- explanation of why it was allowed.
  status        TEXT NOT NULL DEFAULT 'pending',
  chosen_option TEXT,
  resolved_input_json TEXT,
  -- Who answered. Null today because the server is single-user; the column
  -- exists now so multi-user does not need a migration to become auditable.
  resolved_by   TEXT,
  created_at_ms INTEGER NOT NULL,
  resolved_at_ms INTEGER
);

-- The hot query: what is this session waiting on. Partial index because
-- resolved rows accumulate without bound and are never in this answer.
CREATE INDEX pending_requests_open
  ON pending_requests(session_id, created_at_ms)
  WHERE status = 'pending';

-- Resolving arrives with the adapter's id, so that lookup needs its own path.
CREATE INDEX pending_requests_by_adapter_id
  ON pending_requests(adapter_request_id, status);
